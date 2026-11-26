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
    use image::{DynamicImage, ImageBuffer, ImageEncoder, Rgb};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use crate::quick_files::{
        check_free_space, fast_delete, handle_drop, io_hints, pack_solid, pack_zip,
        process_images, unpack_solid, unpack_zip, QuickFileReport,
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
    fn large_image_compress_and_resize() {
        let dir = TestDir::new();
        let img_path = dir.path().join("huge.png");

        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(4000, 3000, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        DynamicImage::ImageRgb8(img).save(&img_path).unwrap();

        let rep = process_images(
            vec![img_path.to_string_lossy().into_owned()],
            "jpg",
            1920,
            82,
            None,
        )
        .unwrap();
        print_speed("4K PNG -> JPG 1920", &rep);
        assert_eq!(rep.count, 1);
        assert_eq!(rep.detail, "1920×1440");
        assert!(Path::new(&rep.output_path).exists());
        assert!(rep.output_bytes < rep.input_bytes);
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
    fn image_scenarios_benchmark() {
        let dir = TestDir::new();

        // 1. 4000x3000 noisy photo-like JPEG (Q85)
        let photo_4k = dir.path().join("photo_4k.jpg");
        {
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(4000, 3000, |x, y| {
                let n = ((x.wrapping_mul(37) ^ y.wrapping_mul(17)) % 23) as u8;
                Rgb([
                    ((x * 255 / 4000) as u8).saturating_add(n),
                    ((y * 255 / 3000) as u8).saturating_add(n),
                    (((x + y) * 127 / 7000) as u8).saturating_add(n),
                ])
            });
            let f = File::create(&photo_4k).unwrap();
            JpegEncoder::new_with_quality(BufWriter::new(f), 85)
                .write_image(img.as_raw(), 4000, 3000, image::ExtendedColorType::Rgb8)
                .unwrap();
        }

        // 2. 1920x1080 already compressed JPEG (Q72)
        let photo_hd_q72 = dir.path().join("photo_hd_q72.jpg");
        {
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(1920, 1080, |x, y| {
                let n = ((x.wrapping_mul(31) ^ y.wrapping_mul(13)) % 29) as u8;
                Rgb([
                    ((x * 255 / 1920) as u8).saturating_add(n),
                    ((y * 255 / 1080) as u8).saturating_add(n),
                    120u8.saturating_add(n),
                ])
            });
            let f = File::create(&photo_hd_q72).unwrap();
            JpegEncoder::new_with_quality(BufWriter::new(f), 72)
                .write_image(img.as_raw(), 1920, 1080, image::ExtendedColorType::Rgb8)
                .unwrap();
        }

        // 3. 1920x1080 RGB PNG (screenshot-like)
        let screenshot_png = dir.path().join("screenshot.png");
        {
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(1920, 1080, |x, y| {
                if (x / 100 + y / 100) % 2 == 0 {
                    Rgb([30, 34, 42])
                } else {
                    Rgb([220, 224, 230])
                }
            });
            let f = File::create(&screenshot_png).unwrap();
            PngEncoder::new_with_quality(BufWriter::new(f), CompressionType::Default, PngFilter::Adaptive)
                .write_image(img.as_raw(), 1920, 1080, image::ExtendedColorType::Rgb8)
                .unwrap();
        }

        for (label, path, mode, dim) in [
            ("4K JPG -> compress 1920", &photo_4k, "compress", 1920),
            ("4K JPG -> webp 1920", &photo_4k, "webp", 1920),
            ("HD Q72 JPG -> compress 1920", &photo_hd_q72, "compress", 1920),
            ("HD PNG -> compress 1920", &screenshot_png, "compress", 1920),
            ("HD PNG -> jpg 1920", &screenshot_png, "jpg", 1920),
            ("HD PNG -> webp 1920", &screenshot_png, "webp", 1920),
        ] {
            let rep = process_images(
                vec![path.to_string_lossy().into_owned()],
                mode,
                dim,
                82,
                None,
            )
            .unwrap();
            print_speed(label, &rep);
            assert!(Path::new(&rep.output_path).exists());
            if mode == "compress" {
                assert!(
                    rep.output_bytes <= rep.input_bytes,
                    "{label} grew: {} -> {}",
                    rep.input_bytes,
                    rep.output_bytes
                );
            }
        }

        // 4. EXIF Orientation = 6 (90° CW): 120x80 raw JPEG should become 80x120
        let exif_jpg = dir.path().join("portrait_exif6.jpg");
        {
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_fn(120, 80, |x, _| Rgb([if x < 60 { 255 } else { 0 }, 0, 0]));
            let mut raw_jpg = Vec::new();
            JpegEncoder::new_with_quality(&mut raw_jpg, 85)
                .write_image(img.as_raw(), 120, 80, image::ExtendedColorType::Rgb8)
                .unwrap();
            let app1: [u8; 32] = [
                0xFF, 0xE1, 0x00, 0x1E, b'E', b'x', b'i', b'f', 0x00, 0x00, b'I', b'I', 0x2A,
                0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00,
                0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
            ];
            let mut with_exif = Vec::with_capacity(raw_jpg.len() + app1.len());
            with_exif.extend_from_slice(&raw_jpg[..2]);
            with_exif.extend_from_slice(&app1);
            with_exif.extend_from_slice(&raw_jpg[2..]);
            fs::write(&exif_jpg, with_exif).unwrap();
        }
        let exif_rep = process_images(
            vec![exif_jpg.to_string_lossy().into_owned()],
            "compress",
            1920,
            82,
            None,
        )
        .unwrap();
        assert_eq!(exif_rep.detail, "80×120");
    }

    #[test]
    fn processes_folder_of_images_and_fast_deletes() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("photos");
        let nested = src.join("sub");
        fs::create_dir_all(&nested).unwrap();

        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(200, 100, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 80]));
        DynamicImage::ImageRgb8(img.clone()).save(src.join("a.png")).unwrap();
        DynamicImage::ImageRgb8(img).save(nested.join("b.png")).unwrap();

        let rep = handle_drop(
            vec![src.to_string_lossy().into_owned()],
            "jpg",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(rep.count, 2);
        let out_dir = Path::new(&rep.output_path);
        assert!(out_dir.is_dir());
        assert!(out_dir.join("a.jpg").exists());
        assert!(out_dir.join("sub").join("b.jpg").exists());

        let del_rep = fast_delete(vec![src.to_string_lossy().into_owned()], None).unwrap();
        print_speed("Fast delete", &del_rep);
        assert_eq!(del_rep.count, 2);
        assert!(!src.exists());

        assert!(fast_delete(vec![r"C:\".into()], None).is_err());
    }

    #[test]
    fn auto_mode_compresses_photo_only_folder_and_zips_mixed_folder() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let photo_dir = dir.path().join("album");
        let mixed_dir = dir.path().join("mixed");
        fs::create_dir_all(&photo_dir).unwrap();
        fs::create_dir_all(&mixed_dir).unwrap();

        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(200, 100, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 40]));
        DynamicImage::ImageRgb8(img.clone()).save(photo_dir.join("1.png")).unwrap();
        DynamicImage::ImageRgb8(img).save(mixed_dir.join("1.png")).unwrap();
        fs::write(mixed_dir.join("notes.txt"), b"hello").unwrap();

        let rep_photo = handle_drop(
            vec![photo_dir.to_string_lossy().into_owned()],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(rep_photo.kind, "image");
        assert!(Path::new(&rep_photo.output_path).is_dir());

        let rep_mixed = handle_drop(
            vec![mixed_dir.to_string_lossy().into_owned()],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(rep_mixed.kind, "zip-pack");
        assert!(rep_mixed.output_path.ends_with(".zip"));
    }

    #[test]
    fn auto_mode_ignores_system_junk_in_photo_folder_and_unpacks_zip() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let album = dir.path().join("camera_roll");
        fs::create_dir_all(&album).unwrap();
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(120, 80, |x, y| Rgb([x as u8, y as u8, 90]));
        DynamicImage::ImageRgb8(img).save(album.join("pic.png")).unwrap();
        fs::write(album.join("Thumbs.db"), b"junk").unwrap();
        fs::write(album.join("desktop.ini"), b"[ViewState]").unwrap();

        let rep = handle_drop(
            vec![album.to_string_lossy().into_owned()],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(rep.kind, "image");
        assert_eq!(rep.count, 1);

        // Auto mode unpacks a single .zip file
        let zip_src = dir.path().join("bundle");
        fs::create_dir_all(&zip_src).unwrap();
        fs::write(zip_src.join("readme.txt"), b"hello world").unwrap();
        let packed = pack_zip(vec![zip_src.to_string_lossy().into_owned()], "normal", None).unwrap();
        let unpacked = handle_drop(vec![packed.output_path], "auto", 1920, 82, "fast", false, None).unwrap();
        assert_eq!(unpacked.kind, "zip-unpack");
        assert_eq!(unpacked.count, 1);
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
        crate::quick_files::handle_drop(vec![], "zip", 0, 82, "fast", false, None).ok();
        match res {
            Err(e) => assert!(e.contains("отменено"), "unexpected err: {e}"),
            Ok(rep) => {
                // If it finished before cancel fired, still ensure no hang and clean up
                let _ = fs::remove_file(&rep.output_path);
            }
        }
    }

    #[test]
    fn handle_drop_delete_after_zip_removes_originals() {
        let _guard = ZIP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TestDir::new();
        let src = dir.path().join("to_archive");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.bin"), vec![1u8; 4096]).unwrap();
        fs::write(src.join("b.txt"), b"hello delete after zip").unwrap();

        let rep = handle_drop(
            vec![src.to_string_lossy().into_owned()],
            "zip",
            1920,
            82,
            "fast",
            true,
            None,
        )
        .unwrap();
        assert_eq!(rep.kind, "zip-pack");
        assert!(rep.deleted_original);
        assert!(!src.exists());
        assert!(Path::new(&rep.output_path).is_file());
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
        let solid_rep = handle_drop(
            vec![src.to_string_lossy().into_owned()],
            "zip",
            1920,
            82,
            "solid",
            false,
            None,
        )
        .unwrap();
        assert!(solid_rep.output_path.ends_with(".tar.zst"));
        assert_eq!(solid_rep.count, 20);
        assert!(solid_rep.output_bytes < fast_rep.output_bytes / 4);

        let unpacked = handle_drop(
            vec![solid_rep.output_path],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
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
        let rep_tgz = handle_drop(
            vec![tgz_path.to_string_lossy().into_owned()],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
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
        let rep_gz = handle_drop(
            vec![gz_path.to_string_lossy().into_owned()],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
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

        let rep = handle_drop(
            vec![
                z.output_path,
                s.output_path,
                tgz_path.to_string_lossy().into_owned(),
            ],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
        assert_eq!(rep.kind, "zip-unpack");
        assert_eq!(rep.count, 3);
        assert_eq!(rep.detail, "3 arch");
        assert_eq!(fs::read_to_string(dir.path().join("alpha (2)/a.txt")).unwrap(), "from zip");
        assert_eq!(fs::read_to_string(dir.path().join("beta (2)/b.txt")).unwrap(), "from solid");
        assert_eq!(fs::read_to_string(dir.path().join("gamma/c.txt")).unwrap(), "from targz");
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

        let rep = handle_drop(
            vec![src.to_string_lossy().into_owned()],
            "zip",
            1920,
            82,
            "ultra",
            false,
            None,
        )
        .unwrap();
        assert!(rep.output_path.ends_with(".tar.zst"));
        assert_eq!(rep.count, 2);
        assert!(rep.output_bytes < rep.input_bytes / 10);

        let unp = handle_drop(
            vec![rep.output_path],
            "auto",
            1920,
            82,
            "fast",
            false,
            None,
        )
        .unwrap();
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

    #[test]
    fn fast_delete_rejects_empty_and_userprofile() {
        assert!(fast_delete(vec![], None).is_err());
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            assert!(fast_delete(vec![profile.to_string_lossy().into_owned()], None).is_err());
        }
    }

    #[test]
    fn transparent_png_preserves_alpha_channel() {
        use image::Rgba;
        let dir = TestDir::new();
        let png_path = dir.path().join("alpha.png");
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(100, 100, |x, _| {
            if x < 50 { Rgba([255, 0, 0, 0]) } else { Rgba([0, 255, 0, 255]) }
        });
        DynamicImage::ImageRgba8(img).save(&png_path).unwrap();

        let rep = process_images(
            vec![png_path.to_string_lossy().into_owned()],
            "compress",
            1920,
            82,
            None,
        )
        .unwrap();
        let out = image::open(&rep.output_path).unwrap();
        assert!(out.color().has_alpha());
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
    use crate::sys_tools::{fix_keyboard_layout, kill_pid, who_locks_paths};

    #[test]
    fn layout_preserves_numbers_and_empty_text() {
        assert_eq!(fix_keyboard_layout(""), "");
        assert_eq!(fix_keyboard_layout("12345"), "12345");
    }

    #[test]
    fn kill_pid_validates_inputs() {
        assert!(kill_pid(0).is_err());
        assert!(kill_pid(std::process::id()).is_err());
    }

    #[test]
    fn who_locks_paths_reports_free_file_and_errors_on_missing() {
        assert!(who_locks_paths(&["Z:\\definitely_missing_123456.txt".into()]).is_err());

        let dir = std::env::temp_dir().join(format!("ml_lock_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("free.txt");
        fs::write(&f, b"ok").unwrap();

        let locks = who_locks_paths(&[f.to_string_lossy().into_owned()]).unwrap();
        assert!(locks.is_empty());

        let _ = fs::remove_dir_all(&dir);
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
