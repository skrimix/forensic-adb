/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Currently the mozdevice API is not safe for multiple requests at the same
// time. It is recommended to run each of the unit tests on its own. Also adb
// specific tests cannot be run in CI yet. To check those locally, also run
// the ignored tests.
//
// Use the following command to accomplish that:
//
//     $ cargo test -- --ignored --test-threads=1

use crate::*;

use futures::future::BoxFuture;
use serial_test::serial;
use std::collections::BTreeSet;
use std::panic;
use std::path::PathBuf;
use std::time::SystemTime;
use tempfile::{tempdir, TempDir};

#[tokio::test]
async fn read_length_from_valid_string() {
    async fn test(message: &str) -> Result<usize> {
        read_length(&mut tokio::io::BufReader::new(message.as_bytes())).await
    }

    assert_eq!(test("0000").await.unwrap(), 0);
    assert_eq!(test("0001").await.unwrap(), 1);
    assert_eq!(test("000F").await.unwrap(), 15);
    assert_eq!(test("00FF").await.unwrap(), 255);
    assert_eq!(test("0FFF").await.unwrap(), 4095);
    assert_eq!(test("FFFF").await.unwrap(), 65535);

    assert_eq!(test("FFFF0").await.unwrap(), 65535);
}

#[tokio::test]
async fn read_length_from_invalid_string() {
    async fn test(message: &str) -> Result<usize> {
        read_length(&mut tokio::io::BufReader::new(message.as_bytes())).await
    }

    test("").await.expect_err("empty string");
    test("G").await.expect_err("invalid hex character");
    test("-1").await.expect_err("negative number");
    test("000").await.expect_err("shorter than 4 bytes");
}

#[test]
fn encode_message_with_valid_string() {
    assert_eq!(encode_message("").unwrap(), "0000".to_string());
    assert_eq!(encode_message("a").unwrap(), "0001a".to_string());
    assert_eq!(
        encode_message(&"a".repeat(15)).unwrap(),
        format!("000F{}", "a".repeat(15))
    );
    assert_eq!(
        encode_message(&"a".repeat(255)).unwrap(),
        format!("00FF{}", "a".repeat(255))
    );
    assert_eq!(
        encode_message(&"a".repeat(4095)).unwrap(),
        format!("0FFF{}", "a".repeat(4095))
    );
    assert_eq!(
        encode_message(&"a".repeat(65535)).unwrap(),
        format!("FFFF{}", "a".repeat(65535))
    );
}

#[test]
fn encode_message_with_invalid_string() {
    encode_message(&"a".repeat(65536)).expect_err("string lengths exceeds 4 bytes");
}

async fn run_device_test<F>(test: F)
where
    F: for<'a> FnOnce(&'a Device, &'a TempDir, &'a UnixPath) -> BoxFuture<'a, ()>
        + panic::UnwindSafe,
{
    let host = Host {
        ..Default::default()
    };
    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::Auto)
        .await
        .expect("device_or_default");

    let tmp_dir = tempdir().expect("create temp dir");
    let response = device
        .execute_host_shell_command("echo $EXTERNAL_STORAGE")
        .await
        .unwrap();
    let mut test_root = UnixPathBuf::from(response.trim_end_matches('\n'));

    test_root.push("mozdevice");

    let _ = device.remove(&test_root).await;

    // TODO: we've removed panic::catch_unwind here, if the test crashes the forwarding isn't cleaned up
    let _result = test(&device, &tmp_dir, &test_root).await;

    let _ = device.kill_forward_all_ports().await;
    // let _ = device.kill_reverse_all_ports();

    // assert!(result.is_ok())
}

#[tokio::test]
async fn host_start_kill_server() {
    let host = Host {
        ..Default::default()
    };

    host.start_server(None).await.expect("to start server");
    host.kill_server(None).await.expect("to kill server");
}

#[tokio::test]
#[ignore]
async fn host_get_host_version() {
    let host = Host {
        ..Default::default()
    };

    let version = host.get_host_version().await.expect("to get host version");
    assert!(version > 0);
}

#[tokio::test]
#[ignore]
async fn host_check_host_running() {
    let host = Host {
        ..Default::default()
    };

    host.check_host_running()
        .await
        .expect("to check host running");
}

#[tokio::test]
#[ignore]
async fn host_features() {
    let host = Host {
        ..Default::default()
    };

    let set = host
        .features::<BTreeSet<_>>()
        .await
        .expect("to query features");
    assert!(set.contains("cmd"));
    assert!(set.contains("shell_v2"));
}

#[tokio::test]
#[ignore]
async fn host_devices() {
    let host = Host {
        ..Default::default()
    };

    let set: BTreeSet<_> = host.devices().await.expect("to query devices");
    assert_eq!(1, set.len());
}

#[tokio::test]
#[ignore]
async fn host_device_or_default() {
    let host = Host {
        ..Default::default()
    };

    let devices: Vec<_> = host.devices().await.expect("to query devices");
    let expected_device = devices.first().expect("found a device");

    let device = host
        .device_or_default::<String>(Some(&expected_device.serial), AndroidStorageInput::App)
        .await
        .expect("connected device with serial");
    assert_eq!(device.run_as_package, None);
    assert_eq!(device.serial, expected_device.serial);
    assert!(device.tempfile.starts_with("/data/local/tmp"));
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_invalid_serial() {
    let host = Host {
        ..Default::default()
    };

    host.device_or_default::<String>(Some(&"foobar".to_owned()), AndroidStorageInput::Auto)
        .await
        .expect_err("invalid serial");
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_no_serial() {
    let host = Host {
        ..Default::default()
    };

    let devices: Vec<_> = host.devices().await.expect("to query devices");
    let expected_device = devices.first().expect("found a device");

    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::Auto)
        .await
        .expect("connected device with serial");
    assert_eq!(device.serial, expected_device.serial);
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_storage_as_app() {
    let host = Host {
        ..Default::default()
    };

    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::App)
        .await
        .expect("connected device");
    assert_eq!(device.storage, AndroidStorage::App);
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_storage_as_auto() {
    let host = Host {
        ..Default::default()
    };

    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::Auto)
        .await
        .expect("connected device");
    assert_eq!(device.storage, AndroidStorage::Sdcard);
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_storage_as_internal() {
    let host = Host {
        ..Default::default()
    };

    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::Internal)
        .await
        .expect("connected device");
    assert_eq!(device.storage, AndroidStorage::Internal);
}

#[tokio::test]
#[ignore]
async fn host_device_or_default_storage_as_sdcard() {
    let host = Host {
        ..Default::default()
    };

    let device = host
        .device_or_default::<String>(None, AndroidStorageInput::Sdcard)
        .await
        .expect("connected device");
    assert_eq!(device.storage, AndroidStorage::Sdcard);
}

#[tokio::test]
#[ignore]
async fn device_shell_command() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            assert_eq!(
                "Linux\n",
                device
                    .execute_host_shell_command("uname")
                    .await
                    .expect("to have shell output")
            );
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(forward)]
async fn device_forward_port_hardcoded() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            assert_eq!(
                3035,
                device
                    .forward_port(3035, 3036)
                    .await
                    .expect("forwarded local port")
            );
            // TODO: check with forward --list
        })
    })
    .await;
}

// #[test]
// #[ignore]
// TODO: "adb server response to `forward tcp:0 ...` was not a u16: \"000559464\"")
// fn device_forward_port_system_allocated() {
//     run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
//         let local_port = device.forward_port(0, 3037).expect("local_port");
//         assert_ne!(local_port, 0);
//         // TODO: check with forward --list
//     });
// }

#[tokio::test]
#[ignore]
#[serial(forward)]
async fn device_kill_forward_port_no_forwarded_port() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            device
                .kill_forward_port(3038)
                .await
                .expect_err("adb error: listener 'tcp:3038' ");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(forward)]
async fn device_kill_forward_port_twice() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            let local_port = device
                .forward_port(3039, 3040)
                .await
                .expect("forwarded local port");
            assert_eq!(local_port, 3039);
            // TODO: check with forward --list
            device
                .kill_forward_port(local_port)
                .await
                .expect("to remove forwarded port");
            device
                .kill_forward_port(local_port)
                .await
                .expect_err("adb error: listener 'tcp:3039' ");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(forward)]
async fn device_kill_forward_all_ports_no_forwarded_port() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            device
                .kill_forward_all_ports()
                .await
                .expect("to not fail for no forwarded ports");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(forward)]
async fn device_kill_forward_all_ports_twice() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            let local_port1 = device
                .forward_port(3039, 3040)
                .await
                .expect("forwarded local port");
            assert_eq!(local_port1, 3039);
            let local_port2 = device
                .forward_port(3041, 3042)
                .await
                .expect("forwarded local port");
            assert_eq!(local_port2, 3041);
            // TODO: check with forward --list
            device
                .kill_forward_all_ports()
                .await
                .expect("to remove all forwarded ports");
            device
                .kill_forward_all_ports()
                .await
                .expect("to not fail for no forwarded ports");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(reverse)]
async fn device_reverse_port_hardcoded() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            assert_eq!(
                4035,
                device.reverse_port(4035, 4036).await.expect("remote_port")
            );
            // TODO: check with reverse --list
        })
    })
    .await;
}

// #[test]
// #[ignore]
// TODO: No adb response: ParseInt(ParseIntError { kind: Empty })
// fn device_reverse_port_system_allocated() {
//     run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
//         let reverse_port = device.reverse_port(0, 4037).expect("remote port");
//         assert_ne!(reverse_port, 0);
//         // TODO: check with reverse --list
//     });
// }

#[tokio::test]
#[ignore]
#[serial(reverse)]
async fn device_kill_reverse_port_no_reverse_port() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            device
                .kill_reverse_port(4038)
                .await
                .expect_err("listener 'tcp:4038' not found");
        })
    })
    .await;
}

// #[test]
// #[ignore]
// TODO: "adb error: adb server response did not contain expected hexstring length: \"\""
// fn device_kill_reverse_port_twice() {
//     run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
//         let remote_port = device
//             .reverse_port(4039, 4040)
//             .expect("reversed local port");
//         assert_eq!(remote_port, 4039);
//         // TODO: check with reverse --list
//         device
//             .kill_reverse_port(remote_port)
//             .expect("to remove reverse port");
//         device
//             .kill_reverse_port(remote_port)
//             .expect_err("listener 'tcp:4039' not found");
//     });
// }

#[tokio::test]
#[ignore]
#[serial(reverse)]
async fn device_kill_reverse_all_ports_no_reversed_port() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            device
                .kill_reverse_all_ports()
                .await
                .expect("to not fail for no reversed ports");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(forward, reverse)]
async fn device_kill_reverse_all_ports_twice() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            let local_port1 = device
                .forward_port(4039, 4040)
                .await
                .expect("forwarded local port");
            assert_eq!(local_port1, 4039);
            let local_port2 = device
                .forward_port(4041, 4042)
                .await
                .expect("forwarded local port");
            assert_eq!(local_port2, 4041);
            // TODO: check with reverse --list
            device
                .kill_reverse_all_ports()
                .await
                .expect("to remove all reversed ports");
            device
                .kill_reverse_all_ports()
                .await
                .expect("to not fail for no reversed ports");
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_pull_text_file() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                let content = "test";
                let remote_path = remote_root_path.join("foo.txt");

                device
                    .push(
                        &mut tokio::io::BufReader::new(content.as_bytes()),
                        &remote_path,
                        0o777,
                    )
                    .await
                    .expect("file has been pushed");

                let file_content = device
                    .execute_host_shell_command(&format!("cat {}", remote_path.display()))
                    .await
                    .expect("host shell command for 'cat' to succeed");

                assert_eq!(file_content, content);

                // And as second step pull it off the device.
                let mut buffer = Vec::new();
                device
                    .pull(&remote_path, &mut buffer)
                    .await
                    .expect("file has been pulled");
                assert_eq!(buffer, content.as_bytes());
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_pull_large_binary_file() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                let remote_path = remote_root_path.join("foo.binary");

                let mut content = Vec::new();

                // Needs to be larger than 64kB to test multiple chunks.
                for i in 0..100000u32 {
                    content.push(b'0' + (i % 10) as u8);
                }

                device
                    .push(
                        &mut std::io::Cursor::new(content.clone()),
                        &remote_path,
                        0o777,
                    )
                    .await
                    .expect("large file has been pushed");

                let output = device
                    .execute_host_shell_command(&format!("ls -l {}", remote_path.display()))
                    .await
                    .expect("host shell command for 'ls' to succeed");

                assert!(output.contains(remote_path.to_str().unwrap()));

                let mut buffer = Vec::new();

                device
                    .pull(&remote_path, &mut buffer)
                    .await
                    .expect("large binary file has been pulled");
                assert_eq!(buffer, content);
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_permission() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                fn adjust_mode(mode: u32) -> u32 {
                    // Adjust the mode by copying the user permissions to
                    // group and other as indicated in
                    // [send_impl](https://android.googlesource.com/platform/system/core/+/master/adb/daemon/file_sync_service.cpp#516).
                    // This ensures that group and other can both access a
                    // file if the user can access it.
                    let mut m = mode & 0o777;
                    m |= (m >> 3) & 0o070;
                    m |= (m >> 3) & 0o007;
                    m
                }

                fn get_permissions(mode: u32) -> String {
                    // Convert the mode integer into the string representation
                    // of the mode returned by `ls`. This assumes the object is
                    // a file and not a directory.
                    let mut perms = ["-", "r", "w", "x", "r", "w", "x", "r", "w", "x"];
                    let mut bit_pos = 0;
                    while bit_pos < 9 {
                        if (1 << bit_pos) & mode == 0 {
                            perms[9 - bit_pos] = "-"
                        }
                        bit_pos += 1;
                    }
                    perms.concat()
                }
                let content = "test";
                let remote_path = remote_root_path.join("foo.bar");

                // First push the file to the device
                let modes = vec![0o421, 0o644, 0o666, 0o777];
                for mode in modes {
                    let adjusted_mode = adjust_mode(mode);
                    let adjusted_perms = get_permissions(adjusted_mode);
                    device
                        .push(
                            &mut tokio::io::BufReader::new(content.as_bytes()),
                            &remote_path,
                            mode,
                        )
                        .await
                        .expect("file has been pushed");

                    let output = device
                        .execute_host_shell_command(&format!("ls -l {}", remote_path.display()))
                        .await
                        .expect("host shell command for 'ls' to succeed");

                    assert!(output.contains(remote_path.to_str().unwrap()));
                    assert!(output.starts_with(&adjusted_perms));
                }

                let output = device
                    .execute_host_shell_command(&format!("ls -ld {}", remote_root_path.display()))
                    .await
                    .expect("host shell command for 'ls parent' to succeed");

                assert!(output.contains(remote_root_path.to_str().unwrap()));
                assert!(output.starts_with("drwxrwxrwx"));
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_pull_fails_for_missing_file() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                let mut buffer = Vec::new();

                device
                    .pull(&remote_root_path.join("missing"), &mut buffer)
                    .await
                    .expect_err("missing file should not be pulled");
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_and_list_dir() {
    run_device_test(
        |device: &Device, tmp_dir: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async move {
                let files = ["foo1.bar", "foo2.bar", "bar/foo3.bar", "bar/more/foo3.bar"];

                for file in files.iter() {
                    let path = tmp_dir.path().join(Path::new(file));
                    let _ = std::fs::create_dir_all(path.parent().unwrap());

                    let f = File::create(path).await.expect("to create file");
                    let mut f = tokio::io::BufWriter::new(f);
                    f.write_all(file.as_bytes()).await.expect("to write data");
                    f.flush().await.expect("to flush data");
                }

                device
                    .push_dir(tmp_dir.path(), remote_root_path, 0o777)
                    .await
                    .expect("to push_dir");

                for file in files.iter() {
                    let path = append_components(remote_root_path, Path::new(file)).unwrap();
                    let output = device
                        .execute_host_shell_command(&format!("ls {}", path.display()))
                        .await
                        .expect("host shell command for 'ls' to succeed");

                    assert!(output.contains(path.to_str().unwrap()));
                }

                let mut listings = device
                    .list_dir(remote_root_path)
                    .await
                    .expect("to list_dir");
                listings.sort_by_key(|f| f.depth);
                // assert_eq!(
                //     listings,
                //     vec![
                //         FileMetadata {
                //             path: "foo1.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 8,
                //             modified_time: None,
                //             depth: Some(0),
                //         },
                //         FileMetadata {
                //             path: "foo2.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 8,
                //             modified_time: None,
                //             depth: Some(0),
                //         },
                //         FileMetadata {
                //             path: "bar".to_string(),
                //             file_mode: UnixFileStatus::Directory,
                //             size: 0,
                //             modified_time: None,
                //             depth: Some(0),
                //         },
                //         FileMetadata {
                //             path: "bar/foo3.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 12,
                //             modified_time: None,
                //             depth: Some(1),
                //         },
                //         FileMetadata {
                //             path: "bar/more".to_string(),
                //             file_mode: UnixFileStatus::Directory,
                //             size: 0,
                //             modified_time: None,
                //             depth: Some(1),
                //         },
                //         FileMetadata {
                //             path: "bar/more/foo3.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 17,
                //             modified_time: None,
                //             depth: Some(2),
                //         }
                //     ]
                // );
                assert_eq!(listings.len(), 6);

                assert_eq!(listings[0].path, "foo1.bar");
                assert_eq!(listings[1].path, "foo2.bar");
                assert_eq!(listings[2].path, "bar");
                assert_eq!(listings[3].path, "bar/foo3.bar");
                assert_eq!(listings[4].path, "bar/more");
                assert_eq!(listings[5].path, "bar/more/foo3.bar");

                assert_eq!(listings[0].file_mode, UnixFileStatus::RegularFile);
                assert_eq!(listings[1].file_mode, UnixFileStatus::RegularFile);
                assert_eq!(listings[2].file_mode, UnixFileStatus::Directory);
                assert_eq!(listings[3].file_mode, UnixFileStatus::RegularFile);
                assert_eq!(listings[4].file_mode, UnixFileStatus::Directory);
                assert_eq!(listings[5].file_mode, UnixFileStatus::RegularFile);

                assert_eq!(listings[0].size, 8);
                assert_eq!(listings[1].size, 8);
                assert_eq!(listings[2].size, 0);
                assert_eq!(listings[3].size, 12);
                assert_eq!(listings[4].size, 0);
                assert_eq!(listings[5].size, 17);

                assert!(listings[0].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[1].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[2].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[3].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[4].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[5].modified_time.unwrap() > SystemTime::UNIX_EPOCH);

                assert_eq!(listings[0].depth, Some(0));
                assert_eq!(listings[1].depth, Some(0));
                assert_eq!(listings[2].depth, Some(0));
                assert_eq!(listings[3].depth, Some(1));
                assert_eq!(listings[4].depth, Some(1));
                assert_eq!(listings[5].depth, Some(2));
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_and_pull_dir() {
    run_device_test(
        |device: &Device, tmp_dir: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async move {
                let files = ["foo1.bar", "foo2.bar", "bar/foo3.bar", "bar/more/foo3.bar"];

                let src_dir = tmp_dir.path().join(Path::new("src"));
                let dest_dir = tmp_dir.path().join(Path::new("src"));

                for file in files.iter() {
                    let path = src_dir.join(Path::new(file));
                    let _ = std::fs::create_dir_all(path.parent().unwrap());

                    let f = File::create(path).await.expect("to create file");
                    let mut f = tokio::io::BufWriter::new(f);
                    f.write_all(file.as_bytes()).await.expect("to write data");
                    f.flush().await.expect("to flush data");
                }

                device
                    .push_dir(&src_dir, remote_root_path, 0o777)
                    .await
                    .expect("to push_dir");

                device
                    .pull_dir(remote_root_path, &dest_dir)
                    .await
                    .expect("to pull_dir");

                for file in files.iter() {
                    let path = dest_dir.join(Path::new(file));
                    let mut f = File::open(path).await.expect("to open file");
                    let mut buf = String::new();
                    f.read_to_string(&mut buf).await.expect("to read content");
                    assert_eq!(buf, *file);
                }
            })
        },
    )
    .await
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_push_and_list_dir_flat() {
    run_device_test(
        |device: &Device, tmp_dir: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async move {
                let content = "test";

                let files = [
                    PathBuf::from("foo1.bar"),
                    PathBuf::from("foo2.bar"),
                    PathBuf::from("bar").join("foo3.bar"),
                ];

                for file in files.iter() {
                    let path = tmp_dir.path().join(file);
                    let _ = std::fs::create_dir_all(path.parent().unwrap());

                    let f = File::create(path).await.expect("to create file");
                    let mut f = tokio::io::BufWriter::new(f);
                    f.write_all(content.as_bytes())
                        .await
                        .expect("to write data");
                    f.flush().await.expect("to flush data");
                }

                device
                    .push_dir(tmp_dir.path(), remote_root_path, 0o777)
                    .await
                    .expect("to push_dir");

                for file in files.iter() {
                    let path = append_components(remote_root_path, file).unwrap();
                    let output = device
                        .execute_host_shell_command(&format!("ls {}", path.display()))
                        .await
                        .expect("host shell command for 'ls' to succeed");

                    assert!(output.contains(path.to_str().unwrap()));
                }

                let mut listings = device
                    .list_dir_flat(remote_root_path, 7, "prefix".to_string())
                    .await
                    .expect("to list_dir_flat");
                listings.sort_by_key(|f| f.depth);
                // assert_eq!(
                //     listings,
                //     vec![
                //         FileMetadata {
                //             path: "prefix/foo1.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 4,
                //             modified_time: None,
                //             depth: Some(7),
                //         },
                //         FileMetadata {
                //             path: "prefix/foo2.bar".to_string(),
                //             file_mode: UnixFileStatus::RegularFile,
                //             size: 4,
                //             modified_time: None,
                //             depth: Some(7),
                //         },
                //         FileMetadata {
                //             path: "prefix/bar".to_string(),
                //             file_mode: UnixFileStatus::Directory,
                //             size: 0,
                //             modified_time: None,
                //             depth: Some(7),
                //         },
                //     ]
                // );
                assert_eq!(listings.len(), 3);
                assert_eq!(listings[0].path, "prefix/foo1.bar");
                assert_eq!(listings[1].path, "prefix/foo2.bar");
                assert_eq!(listings[2].path, "prefix/bar");
                assert_eq!(listings[0].file_mode, UnixFileStatus::RegularFile);
                assert_eq!(listings[1].file_mode, UnixFileStatus::RegularFile);
                assert_eq!(listings[2].file_mode, UnixFileStatus::Directory);
                assert_eq!(listings[0].size, 4);
                assert_eq!(listings[1].size, 4);
                assert_eq!(listings[2].size, 0);
                assert!(listings[0].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[1].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert!(listings[2].modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert_eq!(listings[0].depth, Some(7));
                assert_eq!(listings[1].depth, Some(7));
                assert_eq!(listings[2].depth, Some(7));
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn device_list_packages() {
    run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
        Box::pin(async {
            let packages = device.list_packages(false).await.expect("to list packages");
            assert!(packages.contains(&"com.android.shell".to_owned()));
        })
    })
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_stat_file() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                // First create a test file
                let content = "test content";
                let remote_path = remote_root_path.join("stat_test.txt");

                device
                    .push(
                        &mut tokio::io::BufReader::new(content.as_bytes()),
                        &remote_path,
                        0o644,
                    )
                    .await
                    .expect("file has been pushed");

                // Get file stats
                let stats = device.stat(&remote_path).await.expect("to get file stats");

                assert_eq!(stats.path, remote_path.display().to_string());
                assert_eq!(stats.file_mode, UnixFileStatus::RegularFile);
                assert_eq!(stats.size, content.len() as u32);
                assert!(stats.modified_time.is_some());
                assert!(stats.modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert_eq!(stats.depth, None);
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_stat_directory() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                // Create a test directory
                let remote_dir = remote_root_path.join("stat_test_dir");
                device
                    .create_dir(&remote_dir)
                    .await
                    .expect("to create directory");

                // Get directory stats
                let stats = device
                    .stat(&remote_dir)
                    .await
                    .expect("to get directory stats");

                assert_eq!(stats.path, remote_dir.display().to_string());
                assert_eq!(stats.file_mode, UnixFileStatus::Directory);
                assert!(stats.modified_time.is_some());
                assert!(stats.modified_time.unwrap() > SystemTime::UNIX_EPOCH);
                assert_eq!(stats.depth, None);
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore]
#[serial(file)]
async fn device_stat_nonexistent() {
    run_device_test(
        |device: &Device, _: &TempDir, remote_root_path: &UnixPath| {
            Box::pin(async {
                let nonexistent_path = remote_root_path.join("nonexistent");
                let result = device.stat(&nonexistent_path).await;
                assert!(result.is_err());
                match result {
                    Err(DeviceError::Adb(msg)) => {
                        assert!(msg.contains("No such file or directory"));
                    }
                    _ => panic!("Expected Adb error for nonexistent file"),
                }
            })
        },
    )
    .await;
}

#[test]
fn format_own_device_error_types() {
    assert_eq!(
        format!("{}", DeviceError::InvalidStorage),
        "Invalid storage".to_string()
    );
    assert_eq!(
        format!("{}", DeviceError::MissingPackage),
        "Missing package".to_string()
    );
    assert_eq!(
        format!("{}", DeviceError::MultipleDevices),
        "Multiple Android devices online".to_string()
    );
    assert_eq!(
        format!("{}", DeviceError::PackageManagerError("foo".to_string())),
        "Package manager returned an error: foo".to_string()
    );

    assert_eq!(
        format!("{}", DeviceError::Adb("foo".to_string())),
        "foo".to_string()
    );
}

// #[tokio::test]
// #[ignore]
// async fn device_tcpip() {
//     run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
//         Box::pin(async {
//             device
//                 .clone()
//                 .tcpip(5555)
//                 .await
//                 .expect("to restart adbd in TCP mode");
//         })
//     })
//     .await;
// }

// #[tokio::test]
// #[ignore]
// async fn device_usb() {
//     run_device_test(|device: &Device, _: &TempDir, _: &UnixPath| {
//         Box::pin(async {
//             device
//                 .clone()
//                 .usb()
//                 .await
//                 .expect("to restart adbd in USB mode");
//         })
//     })
//     .await;
// }
