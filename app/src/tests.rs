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

    use crate::quick_files::{fast_delete, handle_drop, pack_zip, process_images, unpack_zip};

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

    #[test]
    fn large_zip_pack_and_unpack_streaming() {
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
        assert_eq!(pack_rep.count, 2);
        assert!(pack_rep.input_bytes > 20 * 1024 * 1024);
        assert!(pack_rep.output_bytes < pack_rep.input_bytes / 2);

        let unpack_rep = unpack_zip(pack_rep.output_path.clone(), None).unwrap();
        assert_eq!(unpack_rep.count, 2);
        assert_eq!(unpack_rep.output_bytes, pack_rep.input_bytes);
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
        assert_eq!(rep.count, 1);
        assert_eq!(rep.detail, "1920×1440");
        assert!(Path::new(&rep.output_path).exists());
        assert!(rep.output_bytes < rep.input_bytes);
    }

    #[test]
    fn unpack_blocks_zip_slip() {
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
            None,
        )
        .unwrap();
        assert_eq!(rep.count, 2);
        let out_dir = Path::new(&rep.output_path);
        assert!(out_dir.is_dir());
        assert!(out_dir.join("a.jpg").exists());
        assert!(out_dir.join("sub").join("b.jpg").exists());

        let del_rep = fast_delete(vec![src.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(del_rep.count, 2);
        assert!(!src.exists());

        assert!(fast_delete(vec![r"C:\".into()], None).is_err());
    }

    #[test]
    fn auto_mode_compresses_photo_only_folder_and_zips_mixed_folder() {
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
            None,
        )
        .unwrap();
        assert_eq!(rep_mixed.kind, "zip-pack");
        assert!(rep_mixed.output_path.ends_with(".zip"));
    }

    #[test]
    fn auto_mode_ignores_system_junk_in_photo_folder_and_unpacks_zip() {
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
        let unpacked = handle_drop(vec![packed.output_path], "auto", 1920, 82, "fast", None).unwrap();
        assert_eq!(unpacked.kind, "zip-unpack");
        assert_eq!(unpacked.count, 1);
    }

    #[test]
    fn zip_modes_store_fast_normal_produce_expected_sizes() {
        let dir = TestDir::new();
        let text_file = dir.path().join("repetitive.txt");
        fs::write(&text_file, "Memory Log compression check line 0123456789\n".repeat(4000)).unwrap();
        let p = vec![text_file.to_string_lossy().into_owned()];

        let store = pack_zip(p.clone(), "store", None).unwrap();
        let fast = pack_zip(p.clone(), "fast", None).unwrap();
        let normal = pack_zip(p, "normal", None).unwrap();

        assert!(fast.output_bytes < store.output_bytes);
        assert!(normal.output_bytes <= fast.output_bytes);
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

        let _ = fs::remove_dir_all(&dir);
    }
}

mod sys_tools_extra {
    use std::fs;
    use crate::sys_tools::{fix_keyboard_layout, kill_pid, open_quick_path, who_locks_paths};

    #[test]
    fn layout_preserves_numbers_and_empty_text() {
        assert_eq!(fix_keyboard_layout(""), "");
        assert_eq!(fix_keyboard_layout("12345"), "12345");
    }

    #[test]
    fn kill_pid_and_open_quick_path_validate_inputs() {
        assert!(kill_pid(0).is_err());
        assert!(kill_pid(std::process::id()).is_err());
        assert!(open_quick_path("invalid_key").is_err());
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
    use crate::sys_tools::fix_keyboard_layout;

    #[test]
    fn fixes_en_and_ru_keyboard_layout() {
        assert_eq!(fix_keyboard_layout("ghbdtn"), "привет");
        assert_eq!(fix_keyboard_layout("руддщ"), "hello");
        assert_eq!(fix_keyboard_layout("Ghbdtn^ rfr ltkf&"), "Привет: как дела?");
    }
}
