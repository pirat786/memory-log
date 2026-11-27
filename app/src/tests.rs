mod i18n {
    use crate::i18n::{dicts, t, t_with, FALLBACK};

    #[test]
    fn every_language_has_the_same_keys_as_english() {
        let d = dicts();
        let en = &d[FALLBACK];
        for (code, dict) in d {
            for key in en.keys() {
                assert!(dict.contains_key(key), "в {code}.json нет ключа {key}");
            }
            for key in dict.keys() {
                assert!(en.contains_key(key), "в {code}.json лишний ключ {key}");
            }
        }
    }

    #[test]
    fn substitutes_params_and_falls_back() {
        assert_eq!(t_with("ru", "err.bad_date", &[("err", "x")]), "неверная дата: x");
        assert_eq!(t("xx", "tray.quit"), "Quit");
        assert_eq!(t("en", "no.such.key"), "no.such.key");
    }
}

mod week_comparison {
    use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
    use ml_core::model::{Category, Goal, Session};

    use crate::week_comparison::{aggregate, timestamp};

    #[test]
    fn comparable_work_hours_clip_sessions_and_count_goal_days() {
        let now = Local.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).single().unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let workday = |hour| NaiveTime::from_hms_opt(hour, 0, 0).unwrap();
        let goals = [Goal { category: Category::Work, limit_mins: 120 }];
        let totals = aggregate(monday, now, Some((workday(9), workday(18))), &goals, |date| {
            if date.weekday() == chrono::Weekday::Tue {
                return Ok(vec![]);
            }
            Ok(vec![Session {
                start: timestamp(date, workday(8)),
                end: timestamp(date, workday(12)),
                app: "editor".into(),
                title: "Document".into(),
                category: Category::Work,
            }])
        })
        .unwrap();
        assert_eq!(totals.through, "2026-11-11");
        assert_eq!(totals.active_secs, 4 * 3600);
        assert_eq!(totals.recorded_days, 2);
        assert_eq!(totals.goals_met[&Category::Work], 1);
    }
}

mod cloud {
    use std::time::Duration;

    use ml_core::ActivityEvent;
    use ml_storage::sync::{Shard, DAY_SECONDS, META_DAY};
    use uuid::Uuid;

    use crate::cloud::google::token_error;
    use crate::cloud::{
        deserialize_shard, filename, next_sync_delay, parse_filename, serialize_shard,
        BACKGROUND_SYNC_INTERVAL, FIRST_TIMEOUT_RETRY, NETWORK_TIMEOUT,
    };

    #[test]
    fn identifies_missing_desktop_client_secret() {
        let response = br#"{"error":"invalid_request","error_description":"client_secret is missing."}"#;
        assert_eq!(token_error(400, response, true), "sync.bad_client");
    }

    #[test]
    fn distinguishes_rejected_sign_in_from_an_expired_connection() {
        let response = br#"{"error":"invalid_grant"}"#;
        assert_eq!(token_error(400, response, true), "sync.code_rejected");
        assert_eq!(token_error(400, response, false), "sync.reauth");
    }

    #[test]
    fn network_timeouts_retry_quickly_then_back_off() {
        let mut next = FIRST_TIMEOUT_RETRY;
        for seconds in [5, 10, 20, 40, 60, 60] {
            assert_eq!(next_sync_delay(Err(NETWORK_TIMEOUT.into()), &mut next), Duration::from_secs(seconds));
        }
        assert_eq!(next_sync_delay(Ok(false), &mut next), BACKGROUND_SYNC_INTERVAL);
        assert_eq!(next_sync_delay(Err(NETWORK_TIMEOUT.into()), &mut next), FIRST_TIMEOUT_RETRY);
        assert_eq!(next_sync_delay(Err("sync.reauth".into()), &mut next), BACKGROUND_SYNC_INTERVAL);
    }

    #[test]
    fn archive_round_trip_and_scope_check() {
        let device = Uuid::new_v4().simple().to_string();
        let shard = Shard {
            protocol: 1,
            device: device.clone(),
            day: 10,
            label: None,
            events: vec![ActivityEvent::Idle(10 * DAY_SECONDS)],
            bookmarks: vec![],
            rules: vec![],
        };
        assert_eq!(parse_filename(&filename(&device, 10)), Some((device.clone(), 10)));
        assert_eq!(parse_filename(&filename(&device, META_DAY)), Some((device.clone(), META_DAY)));
        let bytes = serialize_shard(&shard).unwrap();
        assert_eq!(deserialize_shard(&bytes, &device, 10).unwrap().events, shard.events);
        assert!(deserialize_shard(&bytes, &device, 11).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn distinguishes_winhttp_timeout_from_other_errors() {
        use windows::core::{Error, HRESULT};
        use crate::cloud::windows::http_error;

        assert_eq!(http_error(Error::from_hresult(HRESULT::from_win32(12002))), NETWORK_TIMEOUT);
        assert_ne!(http_error(Error::from_hresult(HRESULT::from_win32(12029))), NETWORK_TIMEOUT);
    }
}

mod quick_files {
    use std::fs::{self, File};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};

    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
    use image::{ImageBuffer, ImageEncoder, Rgb};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use crate::quick_files::{
        check_free_space, handle_archive_drop, io_hints, pack_solid, pack_solid_ultra, pack_zip,
        unpack_solid, unpack_zip, QuickFileReport,
    };

    fn print_speed(label: &str, rep: &QuickFileReport) {
        let in_mb = rep.input_bytes as f64 / 1_048_576.0;
        let out_mb = rep.output_bytes as f64 / 1_048_576.0;
        let secs = (rep.elapsed_ms as f64 / 1000.0).max(0.001);
        let mbps = in_mb.max(out_mb) / secs;
        println!(
            "[SPEED] {label}: {in_mb:.2} MB -> {out_mb:.2} MB | {} files | {} ms | {mbps:.1} MB/s",
            rep.count, rep.elapsed_ms
        );
    }

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("ml_qf_{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    static ZIP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn large_zip_pack_and_unpack_streaming() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src_dir = dir.path().join("payload");
        fs::create_dir_all(&src_dir).unwrap();

        let txt_path = src_dir.join("log.txt");
        {
            let mut w = BufWriter::new(File::create(&txt_path).unwrap());
            let line = b"memory-log quick-drop benchmark line 0123456789abcdef\n";
            for _ in 0..240_000 {
                w.write_all(line).unwrap();
            }
        }
        let jpg_path = src_dir.join("photo.jpg");
        {
            let mut w = BufWriter::new(File::create(&jpg_path).unwrap());
            let chunk = vec![0xABu8; 1024 * 1024];
            for _ in 0..8 {
                w.write_all(&chunk).unwrap();
            }
        }

        let pack_rep = pack_zip(vec![src_dir.to_string_lossy().into_owned()], "fast", None).unwrap();
        print_speed("ZIP pack (fast)", &pack_rep);
        assert_eq!(pack_rep.count, 2);
        assert!(pack_rep.input_bytes > 20 * 1024 * 1024);
        assert!(pack_rep.output_bytes < pack_rep.input_bytes / 2);

        let unpack_rep = unpack_zip(pack_rep.output_path.clone(), None).unwrap();
        print_speed("ZIP unpack", &unpack_rep);
        assert_eq!(unpack_rep.count, 2);
        assert_eq!(unpack_rep.output_bytes, pack_rep.input_bytes);
    }

    #[test]
    #[ignore]
    fn profile_zip_dir_from_env() {
        let log_path = std::env::temp_dir().join(format!("ml_zip_profile_{}.log", uuid::Uuid::new_v4()));
        crate::log::init(log_path.clone());

        if let Ok(arch) = std::env::var("ML_PROFILE_UNPACK_ARCHIVE") {
            let urep = unpack_zip(arch, None).expect("unpack failed");
            print_speed("UNPACK profile", &urep);
            assert!(urep.count > 0);
            let _ = fs::remove_dir_all(&urep.output_path);
            if let Ok(log_contents) = fs::read_to_string(&log_path) {
                println!("=== ZIP PROFILE LOG ===\n{log_contents}=== END LOG ===");
            }
            let _ = fs::remove_file(&log_path);
            return;
        }

        let dir = std::env::var("ML_PROFILE_ZIP_DIR")
            .expect("ML_PROFILE_ZIP_DIR must be set");
        let mode = std::env::var("ML_PROFILE_ZIP_MODE").unwrap_or_else(|_| "fast".into());
        let profile_unpack = std::env::var("ML_PROFILE_UNPACK").is_ok_and(|v| v == "1");
        let keep_archive = std::env::var("ML_PROFILE_KEEP_ARCHIVE").is_ok_and(|v| v == "1");

        let res = if mode == "solid" {
            pack_solid(vec![dir], None)
        } else {
            pack_zip(vec![dir], &mode, None)
        };
        if let Ok(ref rep) = res {
            if mode != "solid" {
                let f = File::open(&rep.output_path).expect("open generated zip");
                let archive = zip::ZipArchive::new(f).expect("read generated zip");
                assert_eq!(archive.len(), rep.count);
                drop(archive);
            }
            if profile_unpack {
                let urep = if mode == "solid" {
                    unpack_solid(rep.output_path.clone(), None)
                } else {
                    unpack_zip(rep.output_path.clone(), None)
                }
                .expect("unpack failed");
                print_speed(&format!("UNPACK profile ({mode})"), &urep);
                assert_eq!(urep.count, rep.count);
                let _ = fs::remove_dir_all(&urep.output_path);
            }
            if keep_archive {
                println!("KEEP_ARCHIVE={}", rep.output_path);
            } else {
                let _ = fs::remove_file(&rep.output_path);
            }
        }
        if let Ok(log_contents) = fs::read_to_string(&log_path) {
            println!("=== ZIP PROFILE LOG ===\n{log_contents}=== END LOG ===");
        }
        let _ = fs::remove_file(&log_path);
        let rep = res.expect("pack failed");
        print_speed(&format!("PACK profile ({mode})"), &rep);
    }

    #[test]
    fn unpack_blocks_zip_slip() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let zip_path = dir.path().join("evil.zip");
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = ZipWriter::new(f);
            zw.start_file("../escape.txt", SimpleFileOptions::default()).unwrap();
            zw.write_all(b"pwned").unwrap();
            zw.start_file("safe.txt", SimpleFileOptions::default()).unwrap();
            zw.write_all(b"ok").unwrap();
            zw.finish().unwrap();
        }
        let rep = unpack_zip(zip_path.to_string_lossy().into_owned(), None).unwrap();
        assert_eq!(rep.count, 1);
        assert!(!dir.path().join("escape.txt").exists());
        assert!(Path::new(&rep.output_path).join("safe.txt").exists());
    }

    #[test]
    fn auto_mode_packs_images_and_files_and_unpacks_archives() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let archive = dir.path().join("archive.zip");
        let mut writer = ZipWriter::new(File::create(&archive).unwrap());
        writer.start_file("inside.txt", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"archive content").unwrap();
        writer.finish().unwrap();

        let photo = dir.path().join("scan.PNG");
        let pixels = (0..256u16)
            .flat_map(|y| (0..256u16).flat_map(move |x| [x as u8, y as u8, 80, (x % 2 * 255) as u8]))
            .collect::<Vec<_>>();
        PngEncoder::new_with_quality(File::create(&photo).unwrap(), CompressionType::Fast, PngFilter::NoFilter)
            .write_image(&pixels, 256, 256, image::ExtendedColorType::Rgba8)
            .unwrap();
        let document = dir.path().join("document.txt");
        fs::write(&document, b"separate document").unwrap();
        let broken = dir.path().join("broken.xz");
        fs::write(&broken, b"unsupported archive").unwrap();

        let result = handle_archive_drop(
            [&archive, &photo, &document, &broken]
                .map(|p| p.to_string_lossy().into_owned())
                .to_vec(),
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("broken.xz"));
        let [unpacked, packed] = result.reports.as_slice() else {
            panic!("expected two operations: unpack + pack, got {:?}", result.reports);
        };
        assert_eq!(unpacked.kind, "zip-unpack");
        assert_eq!(
            fs::read(Path::new(&unpacked.output_path).join("inside.txt")).unwrap(),
            b"archive content"
        );
        assert_eq!(packed.kind, "zip-pack");
        assert!(packed.output_path.ends_with(".zip"));
        let mut packed_zip = zip::ZipArchive::new(File::open(&packed.output_path).unwrap()).unwrap();
        assert_eq!(packed_zip.len(), 2);
        assert!(packed_zip.by_name("scan.PNG").is_ok());
        assert!(packed_zip.by_name("document.txt").is_ok());
    }

    #[test]
    fn ultra_mode_roundtrips_png_jpeg_and_text_with_jxl() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let payload = dir.path().join("media_bundle");
        fs::create_dir_all(&payload).unwrap();

        let png_path = payload.join("кириллица_снимок.png");
        let png_pixels = (0..256u16)
            .flat_map(|y| (0..256u16).flat_map(move |x| [x as u8, y as u8, 100, 255]))
            .collect::<Vec<_>>();
        PngEncoder::new_with_quality(File::create(&png_path).unwrap(), CompressionType::Fast, PngFilter::NoFilter)
            .write_image(&png_pixels, 256, 256, image::ExtendedColorType::Rgba8)
            .unwrap();

        let jpg_path = payload.join("photo.jpg");
        let jpg_img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(320, 240, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut orig_jpg = Vec::new();
        JpegEncoder::new_with_quality(&mut orig_jpg, 90)
            .encode_image(&jpg_img)
            .unwrap();
        fs::write(&jpg_path, &orig_jpg).unwrap();

        let txt_path = payload.join("notes.txt");
        fs::write(&txt_path, b"plain text content inside ultra archive").unwrap();

        let packed = pack_solid_ultra(vec![payload.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(packed.kind, "zip-pack");
        assert!(packed.output_path.ends_with(".tar.zst"));
        assert_eq!(packed.count, 3);

        let unpacked_res = handle_archive_drop(vec![packed.output_path.clone()], "ultra", false, None).unwrap();
        assert!(unpacked_res.errors.is_empty(), "{:?}", unpacked_res.errors);
        assert_eq!(unpacked_res.reports.len(), 1);
        let unpacked = &unpacked_res.reports[0];
        assert_eq!(unpacked.kind, "zip-unpack");
        assert_eq!(unpacked.count, 3);

        let out_dir = Path::new(&unpacked.output_path);
        assert!(!out_dir.join(".memory-log-codecs.json").exists());
        assert!(!out_dir.join("photo.jpg.jxl").exists());
        assert!(!out_dir.join("кириллица_снимок.png.jxl").exists());
        assert_eq!(fs::read(out_dir.join("photo.jpg")).unwrap(), orig_jpg);
        let restored_png = image::open(out_dir.join("кириллица_снимок.png")).unwrap().into_rgba8();
        assert_eq!(restored_png.dimensions(), (256, 256));
        assert_eq!(restored_png.into_raw(), png_pixels);
        assert_eq!(
            fs::read(out_dir.join("notes.txt")).unwrap(),
            b"plain text content inside ultra archive"
        );
    }

    #[test]
    fn solid_unpack_rejects_traversal_in_codec_manifest() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let evil_arch = dir.path().join("evil.tar.zst");
        {
            let f = File::create(&evil_arch).unwrap();
            let enc = zstd::stream::write::Encoder::new(f, 3).unwrap();
            let mut tar = tar::Builder::new(enc);
            let manifest = br#"{"v":1,"jxl":[{"stored":"a.jxl","original":"../escape.png","format":"png"}]}"#;
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, ".memory-log-codecs.json", &manifest[..]).unwrap();
            let enc = tar.into_inner().unwrap();
            enc.finish().unwrap();
        }
        let err = unpack_solid(evil_arch.to_string_lossy().into_owned(), None).unwrap_err();
        assert!(err.contains("небезопасный путь"), "unexpected err: {err}");
        assert!(!dir.path().join("escape.png").exists());
    }

    #[test]
    fn zip_modes_fast_and_max_produce_expected_sizes() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let text_file = dir.path().join("repetitive.txt");
        let raw = "Memory Log compression check line 0123456789\n".repeat(4000);
        fs::write(&text_file, &raw).unwrap();
        let p = vec![text_file.to_string_lossy().into_owned()];

        let fast = pack_zip(p.clone(), "fast", None).unwrap();
        let max = pack_zip(p, "max", None).unwrap();

        assert!(fast.output_bytes < raw.len() as u64);
        assert!(max.output_bytes <= fast.output_bytes);
    }

    #[test]
    fn pack_zip_cancel_does_not_deadlock() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("cancel_payload");
        fs::create_dir_all(&src).unwrap();
        let payload = vec![0x5Au8; 512 * 1024];
        for i in 0..80 {
            fs::write(src.join(format!("img_{i:03}.png")), &payload).unwrap();
        }

        let src_str = src.to_string_lossy().into_owned();
        let t = std::thread::spawn(move || pack_zip(vec![src_str], "fast", None));
        std::thread::sleep(std::time::Duration::from_millis(15));
        crate::quick_files::cancel_drop();
        let res = t.join().expect("pack_zip thread panicked");
        // Reset flag for subsequent tests
        crate::quick_files::handle_archive_drop(vec![], "fast", false, None).ok();
        match res {
            Err(e) => assert!(e.contains("отменено"), "unexpected err: {e}"),
            Ok(rep) => {
                // If it finished before cancel fired, still ensure no hang and clean up
                let _ = fs::remove_file(&rep.output_path);
            }
        }
    }

    #[test]
    fn pack_solid_ultra_cancel_returns_quickly() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("ultra_cancel");
        fs::create_dir_all(&src).unwrap();
        let payload = "Memory Log ultra cancel check 0123456789abcdef\n".repeat(16 * 1024);
        for i in 0..24 {
            fs::write(src.join(format!("chunk_{i:02}.txt")), &payload).unwrap();
        }

        let src_str = src.to_string_lossy().into_owned();
        let started = std::time::Instant::now();
        let t = std::thread::spawn(move || pack_solid_ultra(vec![src_str], None));
        std::thread::sleep(std::time::Duration::from_millis(30));
        crate::quick_files::cancel_drop();
        let res = t.join().expect("pack_solid_ultra thread panicked");
        let elapsed = started.elapsed();
        crate::quick_files::handle_archive_drop(vec![], "fast", false, None).ok();
        assert!(elapsed.as_secs_f64() < 2.0, "cancel took too long: {elapsed:?}");
        if let Ok(rep) = res {
            let _ = fs::remove_file(&rep.output_path);
        }
    }

    #[test]
    fn handle_archive_drop_packs_folder_and_keeps_originals() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("to_archive");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.bin"), vec![1u8; 4096]).unwrap();
        fs::write(src.join("b.txt"), b"hello archive").unwrap();

        let res = handle_archive_drop(vec![src.to_string_lossy().into_owned()], "fast", false, None).unwrap();
        assert!(res.errors.is_empty());
        assert_eq!(res.reports.len(), 1);
        let rep = &res.reports[0];
        assert_eq!(rep.kind, "zip-pack");
        assert!(!rep.deleted_original);
        assert!(src.exists());
        assert!(Path::new(&rep.output_path).is_file());
    }

    #[test]
    fn handle_archive_drop_deletes_originals_when_enabled() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("to_archive_and_delete");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.bin"), vec![1u8; 4096]).unwrap();
        fs::write(src.join("b.txt"), b"hello archive").unwrap();

        let res = handle_archive_drop(vec![src.to_string_lossy().into_owned()], "fast", true, None).unwrap();
        assert!(res.errors.is_empty(), "{:?}", res.errors);
        assert_eq!(res.reports.len(), 1);
        let rep = &res.reports[0];
        assert_eq!(rep.kind, "zip-pack");
        assert!(rep.deleted_original);
        assert!(!src.exists());
        assert!(Path::new(&rep.output_path).is_file());
    }

    #[test]
    fn handle_archive_drop_delete_after_keeps_unpacked_archive() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let arch = dir.path().join("keep_me.zip");
        let mut zip = ZipWriter::new(File::create(&arch).unwrap());
        zip.start_file("inside.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"archive content").unwrap();
        zip.finish().unwrap();

        let res = handle_archive_drop(vec![arch.to_string_lossy().into_owned()], "fast", true, None).unwrap();
        assert!(res.errors.is_empty(), "{:?}", res.errors);
        assert_eq!(res.reports.len(), 1);
        let rep = &res.reports[0];
        assert_eq!(rep.kind, "zip-unpack");
        assert!(!rep.deleted_original);
        assert!(arch.is_file());
    }

    #[test]
    fn pack_zip_preserves_full_folder_name_with_dots() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("scans.2026.part1");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("file.txt"), b"hello").unwrap();

        let rep = pack_zip(vec![src.to_string_lossy().into_owned()], "fast", None).unwrap();
        assert_eq!(
            Path::new(&rep.output_path).file_name(),
            Some(std::ffi::OsStr::new("scans.2026.part1.zip"))
        );
    }

    #[test]
    fn zip_max_mode_compresses_incompressible_extensions() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let exe_file = dir.path().join("dummy.exe");
        let payload = vec![b'A'; 128 * 1024];
        fs::write(&exe_file, &payload).unwrap();
        let p = vec![exe_file.to_string_lossy().into_owned()];

        let fast = pack_zip(p.clone(), "fast", None).unwrap();
        let max = pack_zip(p, "max", None).unwrap();

        assert!(fast.output_bytes >= payload.len() as u64);
        assert!(max.output_bytes < fast.output_bytes / 10);
    }

    #[test]
    fn zstd_max_zip_roundtrip_and_incompressible_fallback() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("zstd_mix");
        fs::create_dir_all(&src).unwrap();

        let exe_data = b"MZ\x90\x00".iter().copied().chain(std::iter::repeat(b'Z').take(128 * 1024)).collect::<Vec<u8>>();
        fs::write(src.join("compressible.exe"), &exe_data).unwrap();

        // High-entropy pseudo-random bytes (>64 KiB) should fall back to Stored in max mode
        let mut jpg_data = vec![0xFF, 0xD8, 0xFF, 0xE0];
        let mut x: u32 = 0x1234_5678;
        for _ in 0..(80 * 1024) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            jpg_data.push((x & 0xFF) as u8);
        }
        fs::write(src.join("noisy.jpg"), &jpg_data).unwrap();

        let rep = pack_zip(vec![src.to_string_lossy().into_owned()], "max", None).unwrap();
        assert!(rep.output_bytes < (exe_data.len() + jpg_data.len()) as u64);
        assert!(rep.output_bytes > jpg_data.len() as u64);

        {
            let f = File::open(&rep.output_path).unwrap();
            let mut arch = zip::ZipArchive::new(f).unwrap();
            assert_eq!(arch.by_name("compressible.exe").unwrap().compression(), zip::CompressionMethod::Zstd);
            assert_eq!(arch.by_name("noisy.jpg").unwrap().compression(), zip::CompressionMethod::Stored);
        }

        let unp = unpack_zip(rep.output_path, None).unwrap();
        let out_dir = Path::new(&unp.output_path);
        assert_eq!(fs::read(out_dir.join("compressible.exe")).unwrap(), exe_data);
        assert_eq!(fs::read(out_dir.join("noisy.jpg")).unwrap(), jpg_data);
    }

    #[test]
    fn solid_tar_zst_pack_and_auto_unpack() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("solid_src");
        let sub = src.join("nested");
        fs::create_dir_all(&sub).unwrap();

        let sample = "Memory Log solid dictionary test payload 0123456789abcdef\n".repeat(400);
        for i in 0..20 {
            let p = if i % 2 == 0 {
                src.join(format!("doc_{i:02}.txt"))
            } else {
                sub.join(format!("log_{i:02}.log"))
            };
            fs::write(p, &sample).unwrap();
        }

        let fast_rep = pack_zip(vec![src.to_string_lossy().into_owned()], "fast", None).unwrap();
        let solid_res =
            handle_archive_drop(vec![src.to_string_lossy().into_owned()], "solid", false, None).unwrap();
        assert!(solid_res.errors.is_empty());
        let solid_rep = solid_res.reports.into_iter().next().unwrap();
        assert!(solid_rep.output_path.ends_with(".tar.zst"));
        assert_eq!(solid_rep.count, 20);
        assert!(solid_rep.output_bytes < fast_rep.output_bytes / 4);

        let unpacked_res = handle_archive_drop(vec![solid_rep.output_path], "fast", false, None).unwrap();
        assert!(unpacked_res.errors.is_empty());
        let unpacked = unpacked_res.reports.into_iter().next().unwrap();
        assert_eq!(unpacked.kind, "zip-unpack");
        assert_eq!(unpacked.count, 20);
        assert_eq!(
            fs::read_to_string(Path::new(&unpacked.output_path).join("doc_00.txt")).unwrap(),
            sample
        );
        assert_eq!(
            fs::read_to_string(Path::new(&unpacked.output_path).join("nested").join("log_01.log")).unwrap(),
            sample
        );
    }

    #[test]
    fn parallel_unpack_zip_restores_many_files() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("many_files");
        fs::create_dir_all(src.join("a/b")).unwrap();
        for i in 0..24 {
            let p = if i % 2 == 0 {
                src.join(format!("f_{i:02}.txt"))
            } else {
                src.join("a/b").join(format!("sub_{i:02}.bin"))
            };
            fs::write(p, format!("payload-{i}-{}", "x".repeat(i * 1024 + 17))).unwrap();
        }

        let packed = pack_zip(vec![src.to_string_lossy().into_owned()], "fast", None).unwrap();
        let unpacked = unpack_zip(packed.output_path, None).unwrap();
        assert_eq!(unpacked.count, 24);
        let out_dir = Path::new(&unpacked.output_path);
        for i in 0..24 {
            let p = if i % 2 == 0 {
                out_dir.join(format!("f_{i:02}.txt"))
            } else {
                out_dir.join("a/b").join(format!("sub_{i:02}.bin"))
            };
            assert_eq!(
                fs::read_to_string(p).unwrap(),
                format!("payload-{i}-{}", "x".repeat(i * 1024 + 17))
            );
        }
    }

    #[test]
    fn zip_and_solid_preserve_empty_dirs_and_mtime() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("with_empty");
        let empty = src.join("sub").join("empty_leaf");
        fs::create_dir_all(&empty).unwrap();
        fs::write(src.join("hello.txt"), b"preserve mtime & empty dir").unwrap();

        let zip_rep = pack_zip(vec![src.to_string_lossy().into_owned()], "fast", None).unwrap();
        {
            let f = File::open(&zip_rep.output_path).unwrap();
            let mut arch = zip::ZipArchive::new(f).unwrap();
            let file_entry = arch.by_name("hello.txt").unwrap();
            assert!(
                file_entry.last_modified().is_some_and(|dt| dt.year() >= 2020),
                "expected preserved mtime year >= 2020"
            );
            drop(file_entry);
            assert!(arch.by_name("sub/empty_leaf/").is_ok());
        }
        let zip_unp = unpack_zip(zip_rep.output_path, None).unwrap();
        assert!(Path::new(&zip_unp.output_path).join("sub/empty_leaf").is_dir());

        let solid_rep = pack_solid(vec![src.to_string_lossy().into_owned()], None).unwrap();
        let solid_unp = unpack_solid(solid_rep.output_path, None).unwrap();
        assert!(Path::new(&solid_unp.output_path).join("sub/empty_leaf").is_dir());
    }

    #[test]
    fn auto_mode_unpacks_tar_gz_and_standalone_gz() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();

        // 1. .tar.gz with nested file and a path-traversal entry
        let tgz_path = dir.path().join("bundle.tar.gz");
        {
            let f = File::create(&tgz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut tb = tar::Builder::new(enc);
            let data = b"hello from tar.gz!";
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tb.append_data(&mut hdr, "nested/hello.txt", &data[..]).unwrap();
            let enc = tb.into_inner().unwrap();
            enc.finish().unwrap();
        }
        let rep_tgz = handle_archive_drop(
            vec![tgz_path.to_string_lossy().into_owned()],
            "fast",
            false,
            None,
        )
        .unwrap()
        .reports
        .remove(0);
        assert_eq!(rep_tgz.kind, "zip-unpack");
        assert_eq!(rep_tgz.count, 1);
        assert_eq!(
            fs::read_to_string(Path::new(&rep_tgz.output_path).join("nested/hello.txt")).unwrap(),
            "hello from tar.gz!"
        );

        // 2. Standalone .txt.gz
        let gz_path = dir.path().join("notes.txt.gz");
        {
            let f = File::create(&gz_path).unwrap();
            let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            enc.write_all(b"standalone gz content").unwrap();
            enc.finish().unwrap();
        }
        let rep_gz = handle_archive_drop(
            vec![gz_path.to_string_lossy().into_owned()],
            "fast",
            false,
            None,
        )
        .unwrap()
        .reports
        .remove(0);
        assert_eq!(rep_gz.kind, "zip-unpack");
        assert_eq!(rep_gz.count, 1);
        assert_eq!(fs::read_to_string(&rep_gz.output_path).unwrap(), "standalone gz content");
    }

    #[test]
    fn disk_space_check_rejects_impossible_size() {
        let tmp = std::env::temp_dir();
        if let Some(avail) = io_hints::available_disk_space(&tmp) {
            assert!(avail > 0);
            let err = check_free_space(&tmp, u64::MAX / 2).unwrap_err();
            assert!(err.contains("недостаточно места"));
        }
    }

    #[test]
    fn auto_mode_unpacks_batch_of_mixed_archives() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();

        let src_a = dir.path().join("alpha");
        let src_b = dir.path().join("beta");
        fs::create_dir_all(&src_a).unwrap();
        fs::create_dir_all(&src_b).unwrap();
        fs::write(src_a.join("a.txt"), b"from zip").unwrap();
        fs::write(src_b.join("b.txt"), b"from solid").unwrap();

        let z = pack_zip(vec![src_a.to_string_lossy().into_owned()], "fast", None).unwrap();
        let s = pack_solid(vec![src_b.to_string_lossy().into_owned()], None).unwrap();

        let tgz_path = dir.path().join("gamma.tar.gz");
        {
            let f = File::create(&tgz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut tb = tar::Builder::new(enc);
            let data = b"from targz";
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tb.append_data(&mut hdr, "c.txt", &data[..]).unwrap();
            tb.into_inner().unwrap().finish().unwrap();
        }

        let result = handle_archive_drop(
            vec![
                z.output_path,
                s.output_path,
                tgz_path.to_string_lossy().into_owned(),
            ],
            "fast",
            false,
            None,
        )
        .unwrap();
        assert!(result.errors.is_empty());
        assert_eq!(result.reports.len(), 3);
        assert!(result.reports.iter().all(|report| report.kind == "zip-unpack" && report.count == 1));
        assert_eq!(fs::read_to_string(dir.path().join("alpha (2)/a.txt")).unwrap(), "from zip");
        assert_eq!(fs::read_to_string(dir.path().join("beta (2)/b.txt")).unwrap(), "from solid");
        assert_eq!(fs::read_to_string(dir.path().join("gamma/c.txt")).unwrap(), "from targz");
    }

    #[test]
    fn auto_mode_recognizes_renamed_zip_and_plain_tar() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let zip_path = dir.path().join("zip-contents.bin");
        let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
        zip.start_file("inside.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"zip").unwrap();
        zip.finish().unwrap();
        let document = dir.path().join("sample.docx");
        fs::copy(&zip_path, &document).unwrap();

        let tar_path = dir.path().join("tar-contents.dat");
        let mut tar = tar::Builder::new(File::create(&tar_path).unwrap());
        let mut header = tar::Header::new_ustar();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "sub/inside.txt", &b"tar"[..]).unwrap();
        tar.finish().unwrap();

        let result = handle_archive_drop(
            [zip_path, tar_path, document].map(|path| path.to_string_lossy().into_owned()).to_vec(),
            "fast",
            false,
            None,
        ).unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.reports.len(), 3);
        assert_eq!(fs::read(Path::new(&result.reports[0].output_path).join("inside.txt")).unwrap(), b"zip");
        assert_eq!(fs::read(Path::new(&result.reports[1].output_path).join("sub/inside.txt")).unwrap(), b"tar");
        let mut packed = zip::ZipArchive::new(File::open(&result.reports[2].output_path).unwrap()).unwrap();
        assert!(packed.by_name("sample.docx").is_ok());
    }


    #[test]
    fn batch_pack_multiple_files_and_folders_zip_and_solid() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let d1 = dir.path().join("d1");
        let d2 = dir.path().join("d2");
        let folder = dir.path().join("sub_folder");
        fs::create_dir_all(&d1).unwrap();
        fs::create_dir_all(&d2).unwrap();
        fs::create_dir_all(&folder).unwrap();

        let f1 = d1.join("same.txt");
        let f2 = d2.join("same.txt");
        let f3 = folder.join("inside.txt");
        fs::write(&f1, b"first").unwrap();
        fs::write(&f2, b"second").unwrap();
        fs::write(&f3, b"folder").unwrap();

        let inputs = vec![
            f1.to_string_lossy().into_owned(),
            f2.to_string_lossy().into_owned(),
            folder.to_string_lossy().into_owned(),
        ];

        let z = pack_zip(inputs.clone(), "fast", None).unwrap();
        assert_eq!(z.count, 3);
        let zu = unpack_zip(z.output_path, None).unwrap();
        assert_eq!(zu.count, 3);
        let zu_dir = Path::new(&zu.output_path);
        assert_eq!(fs::read_to_string(zu_dir.join("same.txt")).unwrap(), "first");
        assert_eq!(fs::read_to_string(zu_dir.join("same (2).txt")).unwrap(), "second");
        assert_eq!(fs::read_to_string(zu_dir.join("sub_folder/inside.txt")).unwrap(), "folder");

        let s = pack_solid(inputs, None).unwrap();
        assert_eq!(s.count, 3);
        let su = unpack_solid(s.output_path, None).unwrap();
        assert_eq!(su.count, 3);
        let su_dir = Path::new(&su.output_path);
        assert_eq!(fs::read_to_string(su_dir.join("same.txt")).unwrap(), "first");
        assert_eq!(fs::read_to_string(su_dir.join("same (2).txt")).unwrap(), "second");
        assert_eq!(fs::read_to_string(su_dir.join("sub_folder/inside.txt")).unwrap(), "folder");
    }

    #[test]
    fn hybrid_solid_multi_frame_roundtrip() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("hybrid_src");
        fs::create_dir_all(&src).unwrap();

        let code = "pub fn hello() { println!(\"world\"); }\n".repeat(1200);
        let json = r#"{"ok":true,"items":[1,2,3,4,5]}"#.repeat(900);
        let mut x: u32 = 0xCAFE_BABE;
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        for _ in 0..(200 * 1024) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            jpg.push((x & 0xFF) as u8);
        }
        let mut mp4 = vec![0x00, 0x00, 0x00, 0x20, b'f', b't', b'y', b'p'];
        for _ in 0..(180 * 1024) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            mp4.push((x & 0xFF) as u8);
        }

        fs::write(src.join("main.rs"), &code).unwrap();
        fs::write(src.join("data.json"), &json).unwrap();
        fs::write(src.join("photo.jpg"), &jpg).unwrap();
        fs::write(src.join("clip.mp4"), &mp4).unwrap();

        let rep = pack_solid(vec![src.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(rep.count, 4);
        assert!(rep.output_bytes < (jpg.len() + mp4.len() + 4096) as u64);

        let unp = unpack_solid(rep.output_path, None).unwrap();
        assert_eq!(unp.count, 4);
        let out_dir = Path::new(&unp.output_path);
        assert_eq!(fs::read_to_string(out_dir.join("main.rs")).unwrap(), code);
        assert_eq!(fs::read_to_string(out_dir.join("data.json")).unwrap(), json);
        assert_eq!(fs::read(out_dir.join("photo.jpg")).unwrap(), jpg);
        assert_eq!(fs::read(out_dir.join("clip.mp4")).unwrap(), mp4);
    }

    #[test]
    fn ultra_tar_zst_pack_and_auto_unpack() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("ultra_src");
        fs::create_dir_all(&src).unwrap();

        let text = "Memory Log ultra preset roundtrip line 0123456789abcdef\n".repeat(1500);
        fs::write(src.join("notes.txt"), &text).unwrap();
        fs::write(src.join("copy.log"), &text).unwrap();

        let rep = handle_archive_drop(
            vec![src.to_string_lossy().into_owned()],
            "ultra",
            false,
            None,
        )
        .unwrap()
        .reports
        .remove(0);
        assert!(rep.output_path.ends_with(".tar.zst"));
        assert_eq!(rep.count, 2);
        assert!(rep.output_bytes < rep.input_bytes / 10);

        let unp = handle_archive_drop(
            vec![rep.output_path],
            "fast",
            false,
            None,
        )
        .unwrap()
        .reports
        .remove(0);
        assert_eq!(unp.kind, "zip-unpack");
        assert_eq!(unp.count, 2);
        let out_dir = Path::new(&unp.output_path);
        assert_eq!(fs::read_to_string(out_dir.join("notes.txt")).unwrap(), text);
        assert_eq!(fs::read_to_string(out_dir.join("copy.log")).unwrap(), text);
    }

    #[test]
    fn solid_probe_and_parallel_pack_unpack_roundtrip() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("solid_par_src");
        fs::create_dir_all(&src).unwrap();

        for i in 0..24 {
            fs::write(
                src.join(format!("item_{i:02}.txt")),
                format!("parallel solid item {i} — {}", "abcd".repeat(64)),
            )
            .unwrap();
        }
        let mut noisy = vec![0u8; 256 * 1024];
        for (i, b) in noisy.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(2654435761)) >> 24) as u8;
        }
        fs::write(src.join("encrypted_like.bin"), &noisy).unwrap();
        let big_text = "large solid stream line 0123456789abcdef\n".repeat(65_000);
        fs::write(src.join("big_stream.txt"), &big_text).unwrap();

        let rep = pack_solid(vec![src.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(rep.count, 26);

        let unp = unpack_solid(rep.output_path, None).unwrap();
        assert_eq!(unp.count, 26);
        let out_dir = Path::new(&unp.output_path);
        for i in 0..24 {
            assert_eq!(
                fs::read_to_string(out_dir.join(format!("item_{i:02}.txt"))).unwrap(),
                format!("parallel solid item {i} — {}", "abcd".repeat(64))
            );
        }
        assert_eq!(fs::read(out_dir.join("encrypted_like.bin")).unwrap(), noisy);
        assert_eq!(fs::read_to_string(out_dir.join("big_stream.txt")).unwrap(), big_text);
    }

}

mod state_widget {
    use std::fs;
    use std::path::PathBuf;
    use crate::state::AppState;

    #[test]
    fn widget_enabled_persists_next_to_db() {
        let dir = std::env::temp_dir().join(format!("ml_state_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db_path: PathBuf = dir.join("memory-log.db");

        let state = AppState::open(&db_path, None).unwrap();
        assert!(!state.widget_enabled());

        state.set_widget_enabled(true);
        assert!(state.widget_enabled());

        let state2 = AppState::open(&db_path, None).unwrap();
        assert!(state2.widget_enabled());

        state2.set_widget_enabled(false);
        assert!(!state2.widget_enabled());

        assert!(!state2.pet_enabled());
        state2.set_pet_enabled(true);
        assert!(state2.pet_enabled());
        assert!(state2.layout_fix_enabled());
        state2.set_layout_fix_enabled(false);
        assert!(!state2.layout_fix_enabled());
        let state3 = AppState::open(&db_path, None).unwrap();
        assert!(state3.pet_enabled());
        assert!(!state3.layout_fix_enabled());
        state3.set_layout_fix_enabled(true);
        assert!(AppState::open(&db_path, None).unwrap().layout_fix_enabled());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn capabilities_include_all_windows() {
        let cap: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let windows = cap["windows"].as_array().unwrap();
        for label in ["main", "popup", "widget", "pet"] {
            assert!(
                windows.iter().any(|w| w.as_str() == Some(label)),
                "в capabilities/default.json нет окна {label}"
            );
        }
    }
}

mod sys_tools_extra {
    use std::fs;
    use std::path::PathBuf;

    use crate::sys_tools::fix_keyboard_layout;
    use crate::sys_tools::lock_watch::{
        extract_candidate_names, file_url_to_path, format_prompt_message, is_in_use_dialog_title,
        is_protected_process, resolve_locked_path, LockingProcess,
    };

    #[test]
    fn layout_preserves_numbers_and_empty_text() {
        assert_eq!(fix_keyboard_layout(""), "");
        assert_eq!(fix_keyboard_layout("12345"), "12345");
    }

    #[test]
    fn lock_watch_matches_ru_and_en_in_use_titles() {
        assert!(is_in_use_dialog_title("Папка уже используется"));
        assert!(is_in_use_dialog_title("  файл используется "));
        assert!(is_in_use_dialog_title("Folder In Use"));
        assert!(is_in_use_dialog_title("File In Use"));
        assert!(!is_in_use_dialog_title("Проводник"));
        assert!(!is_in_use_dialog_title("Удаление файла"));
    }

    #[test]
    fn lock_watch_protects_system_and_self_processes() {
        assert!(is_protected_process("explorer.exe", 1234));
        assert!(is_protected_process("CSRSS.EXE", 555));
        assert!(is_protected_process("notepad.exe", 0));
        assert!(is_protected_process("notepad.exe", 4));
        assert!(is_protected_process("memory-log.exe", std::process::id()));
        assert!(!is_protected_process("Code.exe", 4242));
        assert!(!is_protected_process("python.exe", 9999));
    }

    #[test]
    fn lock_watch_extracts_and_resolves_cyrillic_folder_from_dialog_texts() {
        let root = std::env::temp_dir().join(format!("ml_lw_{}", uuid::Uuid::new_v4()));
        let target = root.join("Фотографии и время (2)");
        fs::create_dir_all(&target).unwrap();

        let dialog_texts = vec![
            "Операция не может быть завершена, так как эти папка или файл\nоткрыты в другой программе"
                .into(),
            "Закройте папку или файл и повторите попытку.".into(),
            "Фотографии и время (2)".into(),
            "Дата создания: 27.11.2026 14:43".into(),
        ];
        let candidates = extract_candidate_names(&dialog_texts);
        assert!(candidates.contains(&"Фотографии и время (2)".to_string()));
        assert!(!candidates.iter().any(|c| c.contains('\n')));

        let resolved = resolve_locked_path(&candidates, &[PathBuf::from("Z:\\missing"), root.clone()]);
        assert_eq!(resolved.as_deref(), Some(target.as_path()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lock_watch_decodes_file_urls_and_formats_prompt() {
        let decoded = file_url_to_path("file:///D:/1/%D0%A4%D0%BE%D1%82%D0%BE").unwrap();
        assert!(decoded.to_string_lossy().contains("Фото"));
        assert!(file_url_to_path("https://example.com").is_none());

        let msg = format_prompt_message(
            std::path::Path::new(r"D:\1\Фото"),
            &[
                LockingProcess {
                    pid: 1234,
                    name: "Code.exe".into(),
                    app_name: "Visual Studio Code".into(),
                },
                LockingProcess {
                    pid: 5678,
                    name: "python.exe".into(),
                    app_name: "python.exe".into(),
                },
            ],
        );
        assert!(msg.contains("Code.exe (Visual Studio Code, PID 1234)"));
        assert!(msg.contains("python.exe (PID 5678)"));
        assert!(msg.contains("Закрыть указанные процессы?"));
    }
}

mod sys_tools {
    use crate::sys_tools::{fix_keyboard_layout, fix_keyboard_layout_with_target};

    #[test]
    fn fixes_en_and_ru_keyboard_layout() {
        assert_eq!(fix_keyboard_layout("ghbdtn"), "привет");
        assert_eq!(fix_keyboard_layout("руддщ"), "hello");
        assert_eq!(fix_keyboard_layout("Ghbdtn^ rfr ltkf&"), "Привет: как дела?");
        assert_eq!(fix_keyboard_layout_with_target("ghbdtn"), ("привет".into(), true));
        assert_eq!(fix_keyboard_layout_with_target("руддщ"), ("hello".into(), false));
    }
}

mod clip_images_tests {
    use ml_storage::Storage;

    #[test]
    fn compresses_1080p_screenshot_into_small_blob_and_thumb() {
        let w = 1920u32;
        let h = 1080u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = ((x * 255) / w) as u8;
                rgba[i + 1] = ((y * 255) / h) as u8;
                rgba[i + 2] = 120;
                rgba[i + 3] = 255;
            }
        }
        let c = crate::clip_images::compress_rgba(w, h, rgba).unwrap();
        assert_eq!(c.width, 1600);
        assert_eq!(c.height, 900);
        assert!(c.thumb.len() < 10_000, "thumb too large: {}", c.thumb.len());
        assert!(c.data.len() < 150_000, "data too large: {}", c.data.len());

        let (dw, dh, decoded) = crate::clip_images::decode_to_rgba(&c.data).unwrap();
        assert_eq!((dw, dh), (1600, 900));
        assert_eq!(decoded.len(), (dw * dh * 4) as usize);

        let st = Storage::open_in_memory().unwrap();
        st.insert_clip_image(100, "snippingtool", c.width, c.height, c.mime, &c.thumb, &c.data, &c.sha256)
            .unwrap();
        // Duplicate sha256 updates ts without creating a second row
        st.insert_clip_image(200, "chrome", c.width, c.height, c.mime, &c.thumb, &c.data, &c.sha256)
            .unwrap();

        let list = st.clip_images_between(0, 300).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].ts, 200);
        assert_eq!(list[0].app, "chrome");
        assert_eq!(list[0].size_bytes, c.data.len() as u64);

        let full = st.clip_image_data(list[0].id).unwrap().unwrap();
        assert_eq!(full.1.len(), c.data.len());

        st.delete_clip_image(list[0].id).unwrap();
        assert!(st.clip_images_between(0, 300).unwrap().is_empty());
    }

    #[test]
    fn searches_clipboard_text_and_images_across_days() {
        let st = Storage::open_in_memory().unwrap();
        st.insert_clip(&ml_core::ClipEntry {
            ts: 100,
            app: "WindowsTerminal".into(),
            text: "cargo build --release".into(),
        })
        .unwrap();
        st.insert_clip(&ml_core::ClipEntry {
            ts: 200_000,
            app: "Telegram".into(),
            text: "Привет мир".into(),
        })
        .unwrap();
        st.insert_clip_image(150, "chrome", 1280, 720, "image/jpeg", b"t1", b"d1", &[1u8; 32]).unwrap();
        st.insert_clip_image(250_000, "code", 1600, 900, "image/jpeg", b"t2", b"d2", &[2u8; 32]).unwrap();

        let text_hits = st.search_clips("привет", 10).unwrap();
        assert_eq!(text_hits.len(), 1);
        assert_eq!(text_hits[0].app, "Telegram");

        let term_hits = st.search_clips("release", 10).unwrap();
        assert_eq!(term_hits.len(), 1);
        assert_eq!(term_hits[0].ts, 100);

        let img_hits = st.search_clip_images("chrome", 10).unwrap();
        assert_eq!(img_hits.len(), 1);
        assert_eq!(img_hits[0].width, 1280);

        let dim_hits = st.search_clip_images("1600x900", 10).unwrap();
        assert_eq!(dim_hits.len(), 1);
        assert_eq!(dim_hits[0].app, "code");
    }
}

mod perf_profile {
    use std::fs;
    use std::time::{Duration, Instant};

    use chrono::{Duration as ChronoDuration, Utc};
    use ml_collector::{interactive_desktop, idle_seconds, LiveSystemProbe, SystemCollector};
    use ml_core::{Categorizer, ClipEntry, ProcSample, Sample, SystemSample};
    use ml_storage::Storage;

    use crate::perf_alloc;
    use crate::state::AppState;

    #[cfg(windows)]
    mod win_perf {
        #[repr(C)]
        #[derive(Default)]
        struct ProcessMemoryCountersEx {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
            private_usage: usize,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentThread() -> isize;
            fn GetCurrentProcess() -> isize;
            fn QueryThreadCycleTime(thread: isize, cycle_time: *mut u64) -> i32;
            fn GetThreadTimes(
                thread: isize,
                creation: *mut u64,
                exit: *mut u64,
                kernel: *mut u64,
                user: *mut u64,
            ) -> i32;
            fn K32GetProcessMemoryInfo(
                process: isize,
                counters: *mut ProcessMemoryCountersEx,
                cb: u32,
            ) -> i32;
        }

        pub fn thread_cycles() -> u64 {
            let mut cycles = 0u64;
            unsafe {
                QueryThreadCycleTime(GetCurrentThread(), &mut cycles);
            }
            cycles
        }

        /// Returns (kernel_us, user_us) for the current thread.
        pub fn thread_cpu_us() -> u64 {
            let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
            unsafe {
                if GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) != 0 {
                    // FILETIME is in 100-nanosecond intervals
                    (k + u) / 10
                } else {
                    0
                }
            }
        }

        /// Returns (working_set_bytes, private_bytes).
        pub fn process_memory_bytes() -> (u64, u64) {
            let mut pmc = ProcessMemoryCountersEx {
                cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
                ..Default::default()
            };
            unsafe {
                if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
                    (pmc.working_set_size as u64, pmc.private_usage as u64)
                } else {
                    (0, 0)
                }
            }
        }
    }

    #[cfg(not(windows))]
    mod win_perf {
        pub fn thread_cycles() -> u64 { 0 }
        pub fn thread_cpu_us() -> u64 { 0 }
        pub fn process_memory_bytes() -> (u64, u64) { (0, 0) }
    }

    struct BenchRow {
        name: &'static str,
        wall_ms: f64,
        cpu_ms: f64,
        mcycles: f64,
        eff_mhz: f64,
        allocs: f64,
        churn_kb: f64,
        retained_kb: f64,
        cpu_pct_at_tick: Option<f64>,
        ws_mb: f64,
        priv_mb: f64,
    }

    fn measure<R>(
        name: &'static str,
        iters: u32,
        tick_secs: Option<f64>,
        mut f: impl FnMut() -> R,
    ) -> BenchRow {
        if iters > 2 {
            let _ = std::hint::black_box(f());
        }
        let alloc_before = perf_alloc::snapshot();
        let cpu_before = win_perf::thread_cpu_us();
        let cycles_before = win_perf::thread_cycles();
        let t0 = Instant::now();

        for _ in 0..iters {
            let r = f();
            std::hint::black_box(r);
        }

        let elapsed = t0.elapsed();
        let cycles_after = win_perf::thread_cycles();
        let cpu_after = win_perf::thread_cpu_us();
        let alloc_after = perf_alloc::snapshot();
        let (ws, priv_b) = win_perf::process_memory_bytes();

        let n = iters.max(1) as f64;
        let wall_us = elapsed.as_secs_f64() * 1_000_000.0 / n;
        let cpu_us = cpu_after.saturating_sub(cpu_before) as f64 / n;
        let cycles = cycles_after.saturating_sub(cycles_before) as f64 / n;
        let eff_mhz = if wall_us > 0.0 { cycles / wall_us } else { 0.0 };
        let allocs = alloc_after.alloc_count.saturating_sub(alloc_before.alloc_count) as f64 / n;
        let churn_b = alloc_after.alloc_bytes.saturating_sub(alloc_before.alloc_bytes) as f64 / n;
        let dealloc_b = alloc_after.dealloc_bytes.saturating_sub(alloc_before.dealloc_bytes) as f64 / n;
        let retained_b = churn_b - dealloc_b;
        let cpu_pct_at_tick = tick_secs.map(|s| (cpu_us / (s * 1_000_000.0)) * 100.0);

        BenchRow {
            name,
            wall_ms: wall_us / 1000.0,
            cpu_ms: cpu_us / 1000.0,
            mcycles: cycles / 1_000_000.0,
            eff_mhz,
            allocs,
            churn_kb: churn_b / 1024.0,
            retained_kb: retained_b / 1024.0,
            cpu_pct_at_tick,
            ws_mb: ws as f64 / (1024.0 * 1024.0),
            priv_mb: priv_b as f64 / (1024.0 * 1024.0),
        }
    }

    fn seed_realistic_db(st: &Storage, days: i64, samples_per_day: i64) {
        let now = Utc::now().timestamp();
        let apps = ["code", "chrome", "Telegram", "WindowsTerminal", "explorer", "notepad"];
        let titles = [
            "main.rs - memory-log - Visual Studio Code",
            "Rust Performance Book - Google Chrome",
            "Saved Messages",
            "PowerShell 7",
            "Downloads",
            "notes.txt - Notepad",
        ];
        for d in 0..days {
            let day_start = now - (d + 1) * 86400;
            for i in 0..samples_per_day {
                let idx = (i as usize) % apps.len();
                let s = Sample {
                    ts: day_start + i * 15,
                    app: apps[idx].to_string(),
                    title: titles[idx].to_string(),
                    idle_secs: 0,
                };
                st.insert_sample(&s).unwrap();
            }
            for i in 0..120 {
                let ts = day_start + i * 300;
                let sys_s = SystemSample {
                    ts,
                    cpu_pct: 18.5,
                    mem_pct: 52.0,
                    mem_used_mb: 8400,
                    net_rx: 120_000,
                    net_tx: 45_000,
                    disk_free_gb: Some(120.5),
                    battery_pct: None,
                    on_battery: None,
                    temp_c: None,
                    uptime_s: 36_000,
                };
                let procs = vec![
                    ProcSample { ts, name: "chrome".into(), cpu_pct: 8.2, mem_mb: 1400 },
                    ProcSample { ts, name: "code".into(), cpu_pct: 5.1, mem_mb: 900 },
                    ProcSample { ts, name: "svchost".into(), cpu_pct: 1.2, mem_mb: 300 },
                ];
                st.insert_system(&sys_s, &procs).unwrap();
            }
        }
        for i in 0..100 {
            st.insert_clip(&ClipEntry {
                ts: now - i * 600,
                app: "code".into(),
                text: format!("clipboard snippet #{i} for search and memory profiling"),
            })
            .unwrap();
        }
    }

    #[test]
    fn profile_hot_paths_and_memory() {
        let dir = std::env::temp_dir().join(format!("ml_perf_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("memory-log.db");

        let mut rows: Vec<BenchRow> = Vec::new();
        let (ws0, priv0) = win_perf::process_memory_bytes();

        // 1. Collector primitives
        rows.push(measure("idle_seconds() [3s]", 200, Some(3.0), || idle_seconds()));
        rows.push(measure("interactive_desktop() [3s]", 50, Some(3.0), || interactive_desktop()));

        let mut win_col = ml_collector::default_collector();
        rows.push(measure("ActiveWindow::sample() [3s]", 50, Some(3.0), || win_col.sample()));

        // 2. SystemCollector (cold + 10s tick)
        let mut sys_col_opt: Option<SystemCollector> = None;
        rows.push(measure("SystemCollector::new() [cold]", 1, None, || {
            let c = SystemCollector::new();
            sys_col_opt = Some(c);
        }));
        let mut sys_col = sys_col_opt.unwrap();
        std::thread::sleep(Duration::from_millis(50));
        rows.push(measure("SystemCollector::snapshot() [10s]", 5, Some(10.0), || sys_col.snapshot()));
        drop(sys_col);

        // 3. LiveSystemProbe (cold + snapshot)
        let mut live_opt: Option<LiveSystemProbe> = None;
        rows.push(measure("LiveSystemProbe::new() [cold]", 1, None, || {
            live_opt = Some(LiveSystemProbe::new());
        }));
        let mut live = live_opt.unwrap();
        rows.push(measure("LiveSystemProbe::snapshot() [ui]", 5, Some(2.0), || live.snapshot()));
        drop(live);

        // 4. Categorizer
        let mut cat_opt: Option<Categorizer> = None;
        rows.push(measure("Categorizer::with_defaults()", 1, None, || {
            cat_opt = Some(Categorizer::with_defaults());
        }));
        let cat = cat_opt.unwrap();
        rows.push(measure("Categorizer::categorize() x1000", 10, None, || {
            let mut acc = 0usize;
            for i in 0..1000 {
                let c = cat.categorize(
                    if i % 2 == 0 { "chrome" } else { "code" },
                    "Rust Performance Book - YouTube",
                );
                acc += c as usize;
            }
            acc
        }));

        // 5. Storage & AppState with 7 days * 1500 samples = 10,500 samples
        let state = AppState::open(&db_path, None).unwrap();
        {
            let st = state.storage.lock().unwrap();
            seed_realistic_db(&st, 7, 1500);
        }

        rows.push(measure("Storage::recording_settings() [3s]", 200, Some(3.0), || {
            state.storage.lock().unwrap().recording_settings().unwrap()
        }));
        let sample_ts = Utc::now().timestamp();
        rows.push(measure("Storage::insert_sample() [3s]", 50, Some(3.0), || {
            state
                .storage
                .lock()
                .unwrap()
                .insert_sample(&Sample {
                    ts: sample_ts,
                    app: "code".into(),
                    title: "tests.rs - memory-log".into(),
                    idle_secs: 0,
                })
                .unwrap()
        }));
        rows.push(measure("AppState::status() [5s]", 100, Some(5.0), || state.status().unwrap()));

        let yesterday = (Utc::now() - ChronoDuration::days(1)).format("%Y-%m-%d").to_string();
        let month_start = Utc::now().format("%Y-%m-01").to_string();

        rows.push(measure("AppState::day_summary() [1.5k]", 10, Some(15.0), || {
            state.day_summary(Some(&yesterday)).unwrap()
        }));
        rows.push(measure("AppState::week_overview() [7d]", 5, None, || {
            state.week_overview().unwrap()
        }));
        rows.push(measure("AppState::calendar_heatmap() [30d]", 3, None, || {
            state.calendar_heatmap(&month_start, None).unwrap()
        }));
        rows.push(measure("AppState::search() [10.5k rows]", 20, None, || {
            state.search("memory").unwrap()
        }));

        let (ws1, priv1) = win_perf::process_memory_bytes();

        println!("\n=== MEMORY LOG PERFORMANCE & RESOURCE PROFILE ===");
        println!(
            "Process RAM: start WS={:.2} MB / Priv={:.2} MB -> end WS={:.2} MB / Priv={:.2} MB",
            ws0 as f64 / 1048576.0,
            priv0 as f64 / 1048576.0,
            ws1 as f64 / 1048576.0,
            priv1 as f64 / 1048576.0,
        );
        println!(
            "{:<38} | {:>8} | {:>8} | {:>8} | {:>7} | {:>9} | {:>9} | {:>10} | {:>7}",
            "Function [tick]", "Wall(ms)", "CPU(ms)", "MCyc", "EffMHz", "Allocs#", "Churn(KB)", "Retain(KB)", "CPU%@tick"
        );
        println!("{}", "-".repeat(122));
        for r in &rows {
            let pct = match r.cpu_pct_at_tick {
                Some(p) => format!("{p:>8.4}%"),
                None => "       - ".to_string(),
            };
            println!(
                "{:<38} | {:>8.3} | {:>8.3} | {:>8.3} | {:>7.1} | {:>9.1} | {:>9.2} | {:>10.2} | {}",
                r.name,
                r.wall_ms,
                r.cpu_ms,
                r.mcycles,
                r.eff_mhz,
                r.allocs,
                r.churn_kb,
                r.retained_kb,
                pct
            );
        }
        println!(
            "Last snapshot RAM: WS={:.2} MB, Private={:.2} MB",
            rows.last().map(|r| r.ws_mb).unwrap_or(0.0),
            rows.last().map(|r| r.priv_mb).unwrap_or(0.0),
        );

        drop(state);
        let _ = fs::remove_dir_all(&dir);
    }
}
