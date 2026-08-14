# forensic-adb

Tokio based client library for the Android Debug Bridge (adb) based on mozdevice for Rust.

[Documentation](https://docs.rs/forensic-adb)

This code has been extracted from [mozilla-central/testing/mozbase/rust/mozdevice][1] and ported to async Rust. It also removes root detection so no commands are executed on the remote device by default.

[1]: https://hg.mozilla.org/mozilla-central/file/tip/testing/mozbase/rust/mozdevice

```rust
use forensic_adb::{DeviceError, Host};

#[tokio::main]
async fn main() -> Result<(), DeviceError> {
    let host = Host::default();

    let devices = host.devices::<Vec<_>>().await?;
    println!("Found devices: {:?}", devices);

    let device = host
        .device_or_default::<String>(None)
        .await?;
    println!("Selected device: {:?}", device);

    let output = device.execute_host_shell_command("id").await?;
    println!("Received response: {:?}", output);

    Ok(())
}
```

Long-running commands can be read as they produce output:

```rust
use forensic_adb::{DeviceError, Host};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), DeviceError> {
    let device = Host::default().device_or_default::<String>(None).await?;
    let output = device.execute_host_shell_command_stream("logcat").await?;
    let mut lines = BufReader::new(output).lines();

    while let Some(line) = lines.next_line().await? {
        println!("{line}");
    }

    Ok(())
}
```

Shell v2 keeps stdout and stderr separate and reports the remote exit code:

```rust
use forensic_adb::{DeviceError, Host};

#[tokio::main]
async fn main() -> Result<(), DeviceError> {
    let device = Host::default().device_or_default::<String>(None).await?;
    let output = device
        .execute_host_shell_v2_command("printf output; printf error >&2; exit 7")
        .await?;

    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    println!("exit code: {}", output.exit_code);
    Ok(())
}
```

Long-running shell v2 commands yield packets as they arrive:

```rust
use forensic_adb::{DeviceError, Host, ShellV2Packet};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), DeviceError> {
    let device = Host::default().device_or_default::<String>(None).await?;
    let mut output = device.execute_host_shell_v2_command_stream("logcat").await?;

    while let Some(packet) = output.next().await {
        match packet? {
            ShellV2Packet::Stdout(bytes) => print!("{}", String::from_utf8_lossy(&bytes)),
            ShellV2Packet::Stderr(bytes) => eprint!("{}", String::from_utf8_lossy(&bytes)),
            ShellV2Packet::Exit(code) => println!("exit code: {code}"),
        }
    }

    Ok(())
}
```

## License

Mozilla Public License (MPL-2.0)
