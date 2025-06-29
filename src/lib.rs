/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

pub mod adb;
pub mod shell;

#[cfg(test)]
pub mod test;

use futures_core::stream::Stream;
use log::{debug, trace, warn};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::io;
use std::iter::FromIterator;
use std::num::{ParseIntError, TryFromIntError};
use std::path::{Component, Path};
use std::str::{FromStr, Utf8Error};
use std::time::{Duration as StdDuration, SystemTime};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{timeout, Duration};
pub use unix_path::{Path as UnixPath, PathBuf as UnixPathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::adb::{DeviceSerial, SyncCommand};

const ADB_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub type Result<T> = std::result::Result<T, DeviceError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnixFileStatus {
    Directory = 0x4000,
    CharacterDevice = 0x2000,
    BlockDevice = 0x6000,
    RegularFile = 0x8000,
    SymbolicLink = 0xA000,
    Socket = 0xC000,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct FileMetadata {
    pub path: String,
    pub file_mode: UnixFileStatus,
    pub size: u32,
    pub modified_time: Option<SystemTime>,
    pub depth: Option<usize>, // Used by list_dir for directory traversal
}

static SYNC_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^A-Za-z0-9_@%+=:,./-]").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum AndroidStorageInput {
    #[default]
    Auto,
    App,
    Internal,
    Sdcard,
}

impl FromStr for AndroidStorageInput {
    type Err = DeviceError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(AndroidStorageInput::Auto),
            "app" => Ok(AndroidStorageInput::App),
            "internal" => Ok(AndroidStorageInput::Internal),
            "sdcard" => Ok(AndroidStorageInput::Sdcard),
            _ => Err(DeviceError::InvalidStorage),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AndroidStorage {
    App,
    Internal,
    Sdcard,
}

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("{0}")]
    Adb(String),
    #[error(transparent)]
    FromInt(#[from] TryFromIntError),
    #[error("Invalid storage")]
    InvalidStorage,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Missing package")]
    MissingPackage,
    #[error("Multiple Android devices online")]
    MultipleDevices,
    #[error(transparent)]
    ParseInt(#[from] ParseIntError),
    #[error("Unknown Android device with serial '{0}'")]
    UnknownDevice(String),
    #[error(transparent)]
    Utf8(#[from] Utf8Error),
    #[error(transparent)]
    WalkDir(#[from] walkdir::Error),
    #[error("Package manager returned an error: {0}")]
    PackageManagerError(String),
    #[error("Timed out while opening ADB connection")]
    ConnectTimeout,
}

fn encode_message(payload: &str) -> Result<String> {
    let hex_length = u16::try_from(payload.len()).map(|len| format!("{:0>4X}", len))?;

    Ok(format!("{}{}", hex_length, payload))
}

fn parse_device_info(line: &str) -> Option<DeviceInfo> {
    // Turn "serial\tdevice key1:value1 key2:value2 ..." into a `DeviceInfo`.
    let mut pairs = line.split_whitespace();
    let serial = pairs.next();
    let state = pairs.next();
    if let (Some(serial), Some(state)) = (serial, state) {
        let info: BTreeMap<String, String> = pairs
            .filter_map(|pair| {
                let mut kv = pair.split(':');
                if let (Some(k), Some(v), None) = (kv.next(), kv.next(), kv.next()) {
                    Some((k.to_owned(), v.to_owned()))
                } else {
                    None
                }
            })
            .collect();

        Some(DeviceInfo {
            serial: serial.to_owned(),
            state: state.into(),
            info,
        })
    } else {
        None
    }
}

fn parse_device_brief(line: &str) -> Option<DeviceBrief> {
    // Turn "serial\tstate" into a `DeviceBrief`.
    let mut pairs = line.split_whitespace();
    let serial = pairs.next();
    let state = pairs.next();
    if let (Some(serial), Some(state)) = (serial, state) {
        Some(DeviceBrief {
            serial: serial.to_owned(),
            state: state.into(),
        })
    } else {
        None
    }
}

/// Reads the payload length of a host message from the stream.
async fn read_length<R: AsyncRead + Unpin>(stream: &mut R) -> Result<usize> {
    let mut bytes: [u8; 4] = [0; 4];
    stream.read_exact(&mut bytes).await?;

    let response = std::str::from_utf8(&bytes)?;

    Ok(usize::from_str_radix(response, 16)?)
}

/// Reads the payload length of a device message from the stream.
async fn read_length_little_endian<R: AsyncRead + Unpin>(reader: &mut R) -> Result<usize> {
    let mut bytes: [u8; 4] = [0; 4];
    reader.read_exact(&mut bytes).await?;

    let n: usize = (bytes[0] as usize)
        + ((bytes[1] as usize) << 8)
        + ((bytes[2] as usize) << 16)
        + ((bytes[3] as usize) << 24);

    Ok(n)
}

/// Writes the payload length of a device message to the stream.
async fn write_length_little_endian<W: AsyncWrite + Unpin>(
    writer: &mut W,
    n: usize,
) -> Result<usize> {
    let mut bytes = [0; 4];
    bytes[0] = (n & 0xFF) as u8;
    bytes[1] = ((n >> 8) & 0xFF) as u8;
    bytes[2] = ((n >> 16) & 0xFF) as u8;
    bytes[3] = ((n >> 24) & 0xFF) as u8;

    writer.write(&bytes[..]).await.map_err(DeviceError::Io)
}

async fn read_response(
    stream: &mut TcpStream,
    has_output: bool,
    has_length: bool,
) -> Result<Vec<u8>> {
    let mut bytes: [u8; 1024] = [0; 1024];

    stream.read_exact(&mut bytes[0..4]).await?;

    if !bytes.starts_with(SyncCommand::Okay.code()) {
        let n = bytes.len().min(read_length(stream).await?);
        stream.read_exact(&mut bytes[0..n]).await?;

        let message = std::str::from_utf8(&bytes[0..n]).map(|s| format!("adb error: {}", s))?;

        return Err(DeviceError::Adb(message));
    }

    let mut response = Vec::new();

    if has_output {
        stream.read_to_end(&mut response).await?;

        if response.starts_with(SyncCommand::Okay.code()) {
            // Sometimes the server produces OKAYOKAY.  Sometimes there is a transport OKAY and
            // then the underlying command OKAY.  This is straight from `chromedriver`.
            response = response.split_off(4);
        }

        if response.starts_with(SyncCommand::Fail.code()) {
            // The server may even produce OKAYFAIL, which means the underlying
            // command failed. First split-off the `FAIL` and length of the message.
            response = response.split_off(8);

            let message = std::str::from_utf8(&response).map(|s| format!("adb error: {}", s))?;

            return Err(DeviceError::Adb(message));
        }

        if has_length {
            if response.len() >= 4 {
                let message = response.split_off(4);
                let slice: &mut &[u8] = &mut &*response;

                let n = read_length(slice).await?;
                if n != message.len() {
                    warn!("adb server response contained hexstring len {} but remaining message length is {}", n, message.len());
                }

                trace!(
                    "adb server response was {:?}",
                    std::str::from_utf8(&message)?
                );

                return Ok(message);
            } else {
                return Err(DeviceError::Adb(format!(
                    "adb server response did not contain expected hexstring length: {:?}",
                    std::str::from_utf8(&response)?
                )));
            }
        }
    }

    Ok(response)
}

/// Information about device connection state.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeviceBrief {
    pub serial: DeviceSerial,
    pub state: DeviceState,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum DeviceState {
    Offline,
    Bootloader,
    Device,
    Host,
    Recovery,
    NoPermissions,
    Sideload,
    Unauthorized,
    Authorizing,
    Unknown,
}

impl From<&str> for DeviceState {
    fn from(s: &str) -> Self {
        match s {
            "offline" => DeviceState::Offline,
            "bootloader" => DeviceState::Bootloader,
            "device" => DeviceState::Device,
            "host" => DeviceState::Host,
            "recovery" => DeviceState::Recovery,
            "no permissions" => DeviceState::NoPermissions,
            "sideload" => DeviceState::Sideload,
            "unauthorized" => DeviceState::Unauthorized,
            "authorizing" => DeviceState::Authorizing,
            "unknown" => DeviceState::Unknown,
            _ => DeviceState::Unknown,
        }
    }
}

/// Detailed information about an ADB device.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeviceInfo {
    pub serial: DeviceSerial,
    pub state: DeviceState,
    pub info: BTreeMap<String, String>,
}

impl From<DeviceInfo> for DeviceBrief {
    fn from(info: DeviceInfo) -> Self {
        DeviceBrief {
            serial: info.serial,
            state: info.state,
        }
    }
}

/// Represents a connection to an ADB host, which multiplexes the connections to
/// individual devices.
#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    /// The TCP host to connect to.  Defaults to `"localhost"`.
    pub host: Option<String>,
    /// The TCP port to connect to.  Defaults to `5037`.
    pub port: Option<u16>,
}

impl Default for Host {
    fn default() -> Host {
        Host {
            host: Some("localhost".to_string()),
            port: Some(5037),
        }
    }
}

impl Host {
    /// Searches for available devices, and selects the one as specified by `device_serial`.
    ///
    /// If multiple devices are online, and no device has been specified,
    /// the `ANDROID_SERIAL` environment variable can be used to select one.
    pub async fn device_or_default<T: AsRef<str>>(
        self,
        device_serial: Option<&T>,
        storage: AndroidStorageInput,
    ) -> Result<Device> {
        let devices: Vec<DeviceInfo> = self
            .devices::<Vec<_>>()
            .await?
            .into_iter()
            .filter(|d| d.state == DeviceState::Device)
            .collect();

        if let Some(ref serial) = device_serial
            .map(|v| v.as_ref().to_owned())
            .or_else(|| std::env::var("ANDROID_SERIAL").ok())
        {
            let device_info = devices.iter().find(|d| d.serial == *serial);
            if let Some(device_info) = device_info {
                return Device::new(
                    self,
                    device_info.serial.to_owned(),
                    device_info.info.clone(),
                    storage,
                )
                .await;
            } else {
                return Err(DeviceError::UnknownDevice(serial.clone()));
            }
        }

        if devices.len() > 1 {
            return Err(DeviceError::MultipleDevices);
        }

        if let Some(device) = devices.first() {
            return Device::new(
                self,
                device.serial.to_owned().to_string(),
                device.info.clone(),
                storage,
            )
            .await;
        }

        Err(DeviceError::Adb("No Android devices are online".to_owned()))
    }

    pub async fn start_server(&self, adb_path: Option<&str>) -> Result<()> {
        let adb_path = adb_path.unwrap_or("adb");
        let mut command = Command::new(adb_path);
        command
            .arg("-H")
            .arg(self.host.clone().unwrap_or("localhost".to_owned()));
        command.arg("-P").arg(self.port.unwrap_or(5037).to_string());
        command.arg("start-server");
        if command.status().await?.success() {
            Ok(())
        } else {
            Err(DeviceError::Adb("Failed to start adb server".to_owned()))
        }
    }

    pub async fn kill_server(&self, adb_path: Option<&str>) -> Result<()> {
        let adb_path = adb_path.unwrap_or("adb");
        let mut command = Command::new(adb_path);
        command
            .arg("-H")
            .arg(self.host.clone().unwrap_or("localhost".to_owned()));
        command.arg("-P").arg(self.port.unwrap_or(5037).to_string());
        command.arg("kill-server");
        if command.status().await?.success() {
            Ok(())
        } else {
            Err(DeviceError::Adb("Failed to kill adb server".to_owned()))
        }
    }

    pub async fn connect(&self) -> Result<TcpStream> {
        let addr = format!(
            "{}:{}",
            self.host.clone().unwrap_or_else(|| "localhost".to_owned()),
            self.port.unwrap_or(5037)
        );

        let stream = timeout(ADB_CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| DeviceError::ConnectTimeout)??;

        stream.set_nodelay(true)?;

        Ok(stream)
    }

    pub async fn execute_command(
        &self,
        command: &str,
        has_output: bool,
        has_length: bool,
    ) -> Result<String> {
        let mut stream = self.connect().await?;

        stream
            .write_all(encode_message(command)?.as_bytes())
            .await?;
        let bytes = read_response(&mut stream, has_output, has_length).await?;
        // TODO: should we assert no bytes were read?

        let response = std::str::from_utf8(&bytes)?;

        Ok(response.to_owned())
    }

    pub async fn execute_host_command(
        &self,
        host_command: &str,
        has_length: bool,
        has_output: bool,
    ) -> Result<String> {
        self.execute_command(&format!("host:{}", host_command), has_output, has_length)
            .await
    }

    pub async fn get_host_version(&self) -> Result<u64> {
        let response = self.execute_host_command("version", true, true).await?;
        if let Ok(version) = u64::from_str_radix(&response, 16) {
            Ok(version)
        } else {
            Err(DeviceError::Adb("Failed to parse host version".to_owned()))
        }
    }

    pub async fn check_host_running(&self) -> Result<()> {
        let version = self.get_host_version().await?;
        if version < 20 {
            Err(DeviceError::Adb("Host version is too old".to_owned()))
        } else {
            Ok(())
        }
    }

    pub async fn features<B: FromIterator<String>>(&self) -> Result<B> {
        let features = self.execute_host_command("features", true, true).await?;
        Ok(features.split(',').map(|x| x.to_owned()).collect())
    }

    pub async fn devices<B: FromIterator<DeviceInfo>>(&self) -> Result<B> {
        let response = self.execute_host_command("devices-l", true, true).await?;

        let infos: B = response.lines().filter_map(parse_device_info).collect();

        Ok(infos)
    }

    pub fn track_devices(&self) -> impl Stream<Item = Result<DeviceBrief>> + '_ {
        async_stream::try_stream! {
            let mut stream = self.connect().await?;
            stream
                .write_all(encode_message("host:track-devices")?.as_bytes())
                .await?;

            let mut bytes: [u8; 1024] = [0; 1024];
            stream.read_exact(&mut bytes[0..4]).await?;
            if !bytes.starts_with(SyncCommand::Okay.code()) {
                let n = bytes.len().min(read_length(&mut stream).await?);
                stream.read_exact(&mut bytes[0..n]).await?;
                let message = std::str::from_utf8(&bytes[0..n]).map(|s| format!("adb error: {}", s))?;
                Err(DeviceError::Adb(message))?;
            }

            loop {
                let length = read_length(&mut stream).await?;

                if length > 0 {
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).await?;
                    if let Some(device) = parse_device_brief(std::str::from_utf8(&body)?) {
                        yield device;
                    }
                    else {
                        Err(DeviceError::Adb("Failed to parse device state".to_owned()))?;
                    }
                }
            }
        }
    }
}

/// Represents an ADB device.
#[derive(Debug, Clone)]
pub struct Device {
    /// ADB host that controls this device.
    pub host: Host,

    /// Serial number uniquely identifying this ADB device.
    pub serial: DeviceSerial,

    /// Information about the device.
    pub info: BTreeMap<String, String>,

    pub run_as_package: Option<String>,

    pub storage: AndroidStorage,

    /// Cache intermediate tempfile name used in pushing via run_as.
    pub tempfile: UnixPathBuf,
}

impl Device {
    pub async fn new(
        host: Host,
        serial: DeviceSerial,
        info: BTreeMap<String, String>,
        storage: AndroidStorageInput,
    ) -> Result<Device> {
        let mut device = Device {
            host,
            serial,
            info,
            run_as_package: None,
            storage: AndroidStorage::App,
            tempfile: UnixPathBuf::from("/data/local/tmp"),
        };
        device
            .tempfile
            .push(Uuid::new_v4().as_hyphenated().to_string());

        device.storage = match storage {
            AndroidStorageInput::App => AndroidStorage::App,
            AndroidStorageInput::Internal => AndroidStorage::Internal,
            AndroidStorageInput::Sdcard => AndroidStorage::Sdcard,
            AndroidStorageInput::Auto => AndroidStorage::Sdcard,
        };

        Ok(device)
    }

    pub async fn clear_app_data(&self, package: &str) -> Result<bool> {
        self.execute_host_shell_command(&format!("pm clear {}", package))
            .await
            .map(|v| v.contains("Success"))
    }

    pub async fn create_dir(&self, path: &UnixPath) -> Result<()> {
        debug!("Creating {}", path.display());

        let enable_run_as = self.enable_run_as_for_path(path);
        self.execute_host_shell_command_as(&format!("mkdir -p {}", path.display()), enable_run_as)
            .await?;

        Ok(())
    }

    pub async fn chmod(&self, path: &UnixPath, mask: &str, recursive: bool) -> Result<()> {
        let enable_run_as = self.enable_run_as_for_path(path);

        let recursive = match recursive {
            true => " -R",
            false => "",
        };

        self.execute_host_shell_command_as(
            &format!("chmod {} {} {}", recursive, mask, path.display()),
            enable_run_as,
        )
        .await?;

        Ok(())
    }

    pub async fn execute_host_command(
        &self,
        command: &str,
        has_output: bool,
        has_length: bool,
    ) -> Result<Vec<u8>> {
        let mut stream = self.host.connect().await?;

        let switch_command = format!("host:transport:{}", self.serial);
        trace!("execute_host_command: >> {:?}", &switch_command);
        stream
            .write_all(encode_message(&switch_command)?.as_bytes())
            .await?;
        let _bytes = read_response(&mut stream, false, false).await?;
        trace!("execute_host_command: << {:?}", _bytes);
        // TODO: should we assert no bytes were read?

        trace!("execute_host_command: >> {:?}", &command);
        stream
            .write_all(encode_message(command)?.as_bytes())
            .await?;
        let bytes = read_response(&mut stream, has_output, has_length).await?;
        trace!("execute_host_command: << {:?}", bstr::BStr::new(&bytes));

        Ok(bytes)
    }

    pub async fn execute_host_command_to_string(
        &self,
        command: &str,
        has_output: bool,
        has_length: bool,
    ) -> Result<String> {
        let bytes = self
            .execute_host_command(command, has_output, has_length)
            .await?;

        let response = std::str::from_utf8(&bytes)?;

        // Unify new lines by removing possible carriage returns
        Ok(response.replace("\r\n", "\n"))
    }

    pub fn enable_run_as_for_path(&self, path: &UnixPath) -> bool {
        match &self.run_as_package {
            Some(package) => {
                let mut p = UnixPathBuf::from("/data/data/");
                p.push(package);
                path.starts_with(p)
            }
            None => false,
        }
    }

    pub async fn execute_host_shell_command(&self, shell_command: &str) -> Result<String> {
        self.execute_host_shell_command_as(shell_command, false)
            .await
    }

    pub async fn execute_host_exec_out_command(&self, shell_command: &str) -> Result<Vec<u8>> {
        self.execute_host_command(&format!("exec:{}", shell_command), true, false)
            .await
    }

    pub async fn execute_host_shell_command_as(
        &self,
        shell_command: &str,
        enable_run_as: bool,
    ) -> Result<String> {
        // We don't want to duplicate su invocations.
        if shell_command.starts_with("su") {
            return self
                .execute_host_command_to_string(&format!("shell:{}", shell_command), true, false)
                .await;
        }

        let has_outer_quotes = shell_command.starts_with('"') && shell_command.ends_with('"')
            || shell_command.starts_with('\'') && shell_command.ends_with('\'');

        // Execute command as package
        if enable_run_as {
            let run_as_package = self
                .run_as_package
                .as_ref()
                .ok_or(DeviceError::MissingPackage)?;

            if has_outer_quotes {
                return self
                    .execute_host_command_to_string(
                        &format!("shell:run-as {} {}", run_as_package, shell_command),
                        true,
                        false,
                    )
                    .await;
            }

            if SYNC_REGEX.is_match(shell_command) {
                let arg: &str = &shell_command.replace('\'', "'\"'\"'")[..];
                return self
                    .execute_host_command_to_string(
                        &format!("shell:run-as {} {}", run_as_package, arg),
                        true,
                        false,
                    )
                    .await;
            }

            return self
                .execute_host_command_to_string(
                    &format!("shell:run-as {} \"{}\"", run_as_package, shell_command),
                    true,
                    false,
                )
                .await;
        }

        self.execute_host_command_to_string(&format!("shell:{}", shell_command), true, false)
            .await
    }

    pub async fn is_app_installed(&self, package: &str) -> Result<bool> {
        self.execute_host_shell_command(&format!("pm path {}", package))
            .await
            .map(|v| v.contains("package:"))
    }

    pub async fn launch<T: AsRef<str>>(
        &self,
        package: &str,
        activity: &str,
        am_start_args: &[T],
    ) -> Result<bool> {
        let mut am_start = format!("am start -W -n {}/{}", package, activity);

        for arg in am_start_args {
            am_start.push(' ');
            if SYNC_REGEX.is_match(arg.as_ref()) {
                am_start.push_str(&format!("\"{}\"", &shell::escape(arg.as_ref())));
            } else {
                am_start.push_str(&shell::escape(arg.as_ref()));
            };
        }

        self.execute_host_shell_command(&am_start)
            .await
            .map(|v| v.contains("Complete"))
    }

    pub async fn force_stop(&self, package: &str) -> Result<()> {
        debug!("Force stopping Android package: {}", package);
        self.execute_host_shell_command(&format!("am force-stop {}", package))
            .await
            .and(Ok(()))
    }

    pub async fn forward_port(&self, local: u16, remote: u16) -> Result<u16> {
        let command = format!(
            "host-serial:{}:forward:tcp:{};tcp:{}",
            self.serial, local, remote
        );
        let response = self.host.execute_command(&command, true, false).await?;

        if local == 0 {
            Ok(response.parse::<u16>()?)
        } else {
            Ok(local)
        }
    }

    pub async fn kill_forward_port(&self, local: u16) -> Result<()> {
        let command = format!("host-serial:{}:killforward:tcp:{}", self.serial, local);
        self.execute_host_command(&command, true, false)
            .await
            .and(Ok(()))
    }

    pub async fn kill_forward_all_ports(&self) -> Result<()> {
        let command = format!("host-serial:{}:killforward-all", self.serial);
        self.execute_host_command(&command, false, false)
            .await
            .and(Ok(()))
    }

    pub async fn reverse_port(&self, remote: u16, local: u16) -> Result<u16> {
        let command = format!("reverse:forward:tcp:{};tcp:{}", remote, local);
        let response = self
            .execute_host_command_to_string(&command, true, false)
            .await?;

        if remote == 0 {
            Ok(response.parse::<u16>()?)
        } else {
            Ok(remote)
        }
    }

    pub async fn kill_reverse_port(&self, remote: u16) -> Result<()> {
        let command = format!("reverse:killforward:tcp:{}", remote);
        self.execute_host_command(&command, true, true)
            .await
            .and(Ok(()))
    }

    pub async fn kill_reverse_all_ports(&self) -> Result<()> {
        let command = "reverse:killforward-all".to_owned();
        self.execute_host_command(&command, false, false)
            .await
            .and(Ok(()))
    }

    pub async fn list_dir(&self, src: &UnixPath) -> Result<Vec<FileMetadata>> {
        let src = src.to_path_buf();
        let mut queue = vec![(src.clone(), 0, "".to_string())];

        let mut listings = Vec::new();

        while let Some((next, depth, prefix)) = queue.pop() {
            for listing in self.list_dir_flat(&next, depth, prefix).await? {
                if listing.file_mode == UnixFileStatus::Directory {
                    let mut child = src.clone();
                    child.push(listing.path.clone());
                    queue.push((child, depth + 1, listing.path.clone()));
                }

                listings.push(listing);
            }
        }

        Ok(listings)
    }

    async fn list_dir_flat(
        &self,
        src: &UnixPath,
        depth: usize,
        prefix: String,
    ) -> Result<Vec<FileMetadata>> {
        // Implement the ADB protocol to list a directory from the device.
        let mut stream = self.host.connect().await?;

        // Send "host:transport" command with device serial
        let message = encode_message(&format!("host:transport:{}", self.serial))?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "sync:" command to initialize file transfer
        let message = encode_message("sync:")?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "LIST" command with name of the directory
        stream.write_all(SyncCommand::List.code()).await?;
        let args_ = format!("{}", src.display());
        let args = args_.as_bytes();
        write_length_little_endian(&mut stream, args.len()).await?;
        stream.write_all(args).await?;

        // Use the maximum 64K buffer to transfer the file contents.
        let mut buf = [0; 64 * 1024];

        let mut listings = Vec::new();

        // Read "DENT" command one or more times for the directory entries
        loop {
            stream.read_exact(&mut buf[0..4]).await?;

            if &buf[0..4] == SyncCommand::Dent.code() {
                // From https://github.com/cstyan/adbDocumentation/blob/6d025b3e4af41be6f93d37f516a8ac7913688623/README.md:
                //
                // A four-byte integer representing file mode - first 9 bits of this mode represent
                // the file permissions, as with chmod mode. Bits 14 to 16 seem to represent the
                // file type, one of 0b100 (file), 0b010 (directory), 0b101 (symlink)
                // A four-byte integer representing file size.
                // A four-byte integer representing last modified time in seconds since Unix Epoch.
                // A four-byte integer representing file name length.
                // A utf-8 string representing the file name.
                let mode = read_length_little_endian(&mut stream).await?;
                let size = read_length_little_endian(&mut stream).await?;
                let time = read_length_little_endian(&mut stream).await?;
                let mod_time = SystemTime::UNIX_EPOCH + StdDuration::from_secs(time as u64);
                let name_length = read_length_little_endian(&mut stream).await?;
                stream.read_exact(&mut buf[0..name_length]).await?;

                let mut name = std::str::from_utf8(&buf[0..name_length])?.to_owned();

                if name == "." || name == ".." {
                    continue;
                }

                if !prefix.is_empty() {
                    name = format!("{}/{}", prefix, &name);
                }

                let file_type = (mode >> 13) & 0b111;
                let metadata = match file_type {
                    0b010 => FileMetadata {
                        path: name,
                        file_mode: UnixFileStatus::Directory,
                        size: 0,
                        modified_time: Some(mod_time),
                        depth: Some(depth),
                    },
                    0b100 => FileMetadata {
                        path: name,
                        file_mode: UnixFileStatus::RegularFile,
                        size: size as u32,
                        modified_time: Some(mod_time),
                        depth: Some(depth),
                    },
                    0b101 => FileMetadata {
                        path: name,
                        file_mode: UnixFileStatus::SymbolicLink,
                        size: 0,
                        modified_time: Some(mod_time),
                        depth: Some(depth),
                    },
                    _ => return Err(DeviceError::Adb(format!("Invalid file mode {}", file_type))),
                };

                listings.push(metadata);
            } else if &buf[0..4] == SyncCommand::Done.code() {
                // "DONE" command indicates end of file transfer
                break;
            } else if &buf[0..4] == SyncCommand::Fail.code() {
                let n = buf.len().min(read_length_little_endian(&mut stream).await?);

                stream.read_exact(&mut buf[0..n]).await?;

                let message = std::str::from_utf8(&buf[0..n])
                    .map(|s| format!("adb error: {}", s))
                    .unwrap_or_else(|_| "adb error was not utf-8".into());

                return Err(DeviceError::Adb(message));
            } else {
                return Err(DeviceError::Adb("FAIL (unknown)".to_owned()));
            }
        }

        Ok(listings)
    }

    pub async fn path_exists(&self, path: &UnixPath, enable_run_as: bool) -> Result<bool> {
        self.execute_host_shell_command_as(format!("ls {}", path.display()).as_str(), enable_run_as)
            .await
            .map(|path| !path.contains("No such file or directory"))
    }

    pub async fn pull<W: AsyncWrite + Unpin>(&self, src: &UnixPath, buffer: &mut W) -> Result<()> {
        self.pull_internal(src, buffer, None, None).await
    }

    pub async fn pull_with_progress<W: AsyncWrite + Unpin>(
        &self,
        src: &UnixPath,
        buffer: &mut W,
        progress_sender: UnboundedSender<FileTransferProgress>,
    ) -> Result<()> {
        let metadata = self.stat(src).await?;
        let total_bytes = metadata.size as u64;

        self.pull_internal(src, buffer, Some(total_bytes), Some(progress_sender))
            .await
    }

    async fn pull_internal<W: AsyncWrite + Unpin>(
        &self,
        src: &UnixPath,
        buffer: &mut W,
        total_bytes: Option<u64>,
        progress_sender: Option<UnboundedSender<FileTransferProgress>>,
    ) -> Result<()> {
        if let (Some(total), Some(sender)) = (total_bytes, &progress_sender) {
            let _ = sender.send(FileTransferProgress {
                total_bytes: total,
                transferred_bytes: 0,
            });
        }

        let mut stream = self.host.connect().await?;

        // Send "host:transport" command with device serial
        let message = encode_message(&format!("host:transport:{}", self.serial))?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "sync:" command to initialize file transfer
        let message = encode_message("sync:")?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "RECV" command with name of the file
        stream.write_all(SyncCommand::Recv.code()).await?;
        let args_string = format!("{}", src.display());
        let args = args_string.as_bytes();
        write_length_little_endian(&mut stream, args.len()).await?;
        stream.write_all(args).await?;

        // Use the maximum 64K buffer to transfer the file contents.
        let mut buf = [0; 64 * 1024];
        let mut transferred = 0u64;
        let mut last_progress = 0u64;

        // Read "DATA" command one or more times for the file content
        loop {
            stream.read_exact(&mut buf[0..4]).await?;

            if &buf[0..4] == SyncCommand::Data.code() {
                let len = read_length_little_endian(&mut stream).await?;
                stream.read_exact(&mut buf[0..len]).await?;
                buffer.write_all(&buf[0..len]).await?;

                transferred += len as u64;

                // Send progress every 1M if progress reporting is enabled
                if let Some(sender) = &progress_sender {
                    if transferred - last_progress >= 1024 * 1024 {
                        let _ = sender.send(FileTransferProgress {
                            total_bytes: total_bytes.unwrap_or(0),
                            transferred_bytes: transferred,
                        });
                        last_progress = transferred;
                    }
                }
            } else if &buf[0..4] == SyncCommand::Done.code() {
                // "DONE" command indicates end of file transfer
                if let Some(sender) = &progress_sender {
                    let _ = sender.send(FileTransferProgress {
                        total_bytes: total_bytes.unwrap_or(0),
                        transferred_bytes: transferred,
                    });
                }
                break;
            } else if &buf[0..4] == SyncCommand::Fail.code() {
                let n = buf.len().min(read_length_little_endian(&mut stream).await?);

                stream.read_exact(&mut buf[0..n]).await?;

                let message = std::str::from_utf8(&buf[0..n])
                    .map(|s| format!("adb error: {}", s))
                    .unwrap_or_else(|_| "adb error was not utf-8".into());

                return Err(DeviceError::Adb(message));
            } else {
                return Err(DeviceError::Adb("FAIL (unknown)".to_owned()));
            }
        }

        Ok(())
    }

    pub async fn pull_dir(&self, src: &UnixPath, dest_dir: &Path) -> Result<()> {
        self.pull_dir_internal(src, dest_dir, None).await
    }

    async fn pull_dir_internal(
        &self,
        src: &UnixPath,
        dest_dir: &Path,
        progress_sender: Option<UnboundedSender<DirectoryTransferProgress>>,
    ) -> Result<()> {
        let src = src.to_path_buf();
        let dest_dir = dest_dir.to_path_buf();

        // Get totals first
        let mut total_files = 0usize;
        let mut total_bytes = 0u64;
        for entry in self.list_dir(&src).await? {
            if entry.file_mode == UnixFileStatus::RegularFile {
                total_files += 1;
                total_bytes += entry.size as u64;
            }
        }

        // Send initial progress if progress reporting is enabled
        if let Some(sender) = &progress_sender {
            let _ = sender.send(DirectoryTransferProgress {
                directory_name: Some(src.display().to_string()),
                total_files,
                transferred_files: 0,
                total_bytes,
                transferred_bytes: 0,
                current_file: None,
                current_file_progress: FileTransferProgress {
                    total_bytes: 0,
                    transferred_bytes: 0,
                },
            });
        }

        let mut transferred_files = 0usize;
        let mut transferred_bytes = 0u64;

        for entry in self.list_dir(&src).await? {
            match entry.file_mode {
                UnixFileStatus::SymbolicLink => {} // Ignored
                UnixFileStatus::Directory => {
                    let mut d = dest_dir.clone();
                    d.push(&entry.path);
                    std::fs::create_dir_all(&d)?;
                }
                UnixFileStatus::RegularFile => {
                    let mut s = src.clone();
                    s.push(&entry.path);
                    let mut d = dest_dir.clone();
                    d.push(&entry.path);

                    let file_size = entry.size as u64;

                    // Create a channel for file progress if directory progress is enabled
                    let (file_sender, mut file_receiver): (
                        Option<UnboundedSender<FileTransferProgress>>,
                        Option<UnboundedReceiver<FileTransferProgress>>,
                    ) = progress_sender
                        .as_ref()
                        .map(|_| tokio::sync::mpsc::unbounded_channel())
                        .map(|(s, r)| (Some(s), Some(r)))
                        .unwrap_or((None, None));

                    // Send directory progress with current file
                    if let Some(sender) = &progress_sender {
                        let _ = sender.send(DirectoryTransferProgress {
                            directory_name: None,
                            total_files,
                            transferred_files,
                            total_bytes,
                            transferred_bytes,
                            current_file: Some(d.display().to_string()),
                            current_file_progress: FileTransferProgress {
                                total_bytes: file_size,
                                transferred_bytes: 0,
                            },
                        });

                        // Spawn a task to handle file progress updates if progress reporting is enabled
                        if let Some(mut receiver) = file_receiver.take() {
                            let sender = sender.clone();
                            tokio::spawn(async move {
                                while let Some(file_progress) = receiver.recv().await {
                                    let _ = sender.send(DirectoryTransferProgress {
                                        directory_name: None,
                                        total_files,
                                        transferred_files,
                                        total_bytes,
                                        transferred_bytes: transferred_bytes
                                            + file_progress.transferred_bytes,
                                        current_file: None,
                                        current_file_progress: file_progress,
                                    });
                                }
                            });
                        }
                    }

                    // Pull file with progress if enabled
                    self.pull_internal(
                        &s,
                        &mut File::create(&d).await?,
                        Some(file_size),
                        file_sender,
                    )
                    .await?;

                    transferred_files += 1;
                    transferred_bytes += file_size;
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub async fn push<R: AsyncRead + Unpin>(
        &self,
        buffer: &mut R,
        dest: &UnixPath,
        mode: u32,
    ) -> Result<()> {
        self.push_internal(buffer, dest, mode, None, None).await
    }

    pub async fn push_with_progress<R: AsyncRead + Unpin>(
        &self,
        buffer: &mut R,
        dest: &UnixPath,
        mode: u32,
        total_bytes: u64,
        progress_sender: UnboundedSender<FileTransferProgress>,
    ) -> Result<()> {
        self.push_internal(buffer, dest, mode, Some(total_bytes), Some(progress_sender))
            .await
    }

    async fn push_internal<R: AsyncRead + Unpin>(
        &self,
        buffer: &mut R,
        dest: &UnixPath,
        mode: u32,
        total_bytes: Option<u64>,
        progress_sender: Option<UnboundedSender<FileTransferProgress>>,
    ) -> Result<()> {
        // Implement the ADB protocol to send a file to the device.
        // The protocol consists of the following steps:
        // * Send "host:transport" command with device serial
        // * Send "sync:" command to initialize file transfer
        // * Send "SEND" command with name and mode of the file
        // * Send "DATA" command one or more times for the file content
        // * Send "DONE" command to indicate end of file transfer
        if let (Some(total), Some(sender)) = (total_bytes, &progress_sender) {
            let _ = sender.send(FileTransferProgress {
                total_bytes: total,
                transferred_bytes: 0,
            });
        }

        let enable_run_as = self.enable_run_as_for_path(&dest.to_path_buf());
        let dest1 = match enable_run_as {
            true => self.tempfile.as_path(),
            false => UnixPath::new(dest),
        };

        // If the destination directory does not exist, adb will
        // create it and any necessary ancestors however it will not
        // set the directory permissions to 0o777.  In addition,
        // Android 9 (P) has a bug in its push implementation which
        // will cause a push which creates directories to fail with
        // the error `secure_mkdirs failed: Operation not
        // permitted`. We can work around this by creating the
        // destination directories prior to the push.  Collect the
        // ancestors of the destination directory which do not yet
        // exist so we can create them and adjust their permissions
        // prior to performing the push.
        let mut current = dest.parent();
        let mut leaf: Option<&UnixPath> = None;
        let mut root: Option<&UnixPath> = None;

        while let Some(path) = current {
            if self.path_exists(path, enable_run_as).await? {
                break;
            }
            if leaf.is_none() {
                leaf = Some(path);
            }
            root = Some(path);
            current = path.parent();
        }

        if let Some(path) = leaf {
            self.create_dir(path).await?;
        }

        if let Some(path) = root {
            self.chmod(path, "777", true).await?;
        }

        let mut stream = self.host.connect().await?;

        let message = encode_message(&format!("host:transport:{}", self.serial))?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        let message = encode_message("sync:")?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        stream.write_all(SyncCommand::Send.code()).await?;
        let args_ = format!("{},{}", dest1.display(), mode);
        let args = args_.as_bytes();
        write_length_little_endian(&mut stream, args.len()).await?;
        stream.write_all(args).await?;

        // Use a 32K buffer to transfer the file contents
        // TODO: Maybe adjust to maxdata (256K)
        let mut buf = [0; 32 * 1024];
        let mut transferred = 0u64;
        let mut last_progress = 0u64;

        loop {
            let len = buffer.read(&mut buf).await?;
            if len == 0 {
                // We're done, send the final progress update
                if let Some(sender) = &progress_sender {
                    let _ = sender.send(FileTransferProgress {
                        total_bytes: total_bytes.unwrap_or(0),
                        transferred_bytes: transferred,
                    });
                }
                break;
            }

            stream.write_all(SyncCommand::Data.code()).await?;
            write_length_little_endian(&mut stream, len).await?;
            stream.write_all(&buf[0..len]).await?;

            transferred += len as u64;

            // Send progress every 4M if progress reporting is enabled
            if let Some(sender) = &progress_sender {
                if transferred - last_progress >= 4 * 1024 * 1024 {
                    let _ = sender.send(FileTransferProgress {
                        total_bytes: total_bytes.unwrap_or(0),
                        transferred_bytes: transferred,
                    });
                    last_progress = transferred;
                }
            }
        }

        // https://android.googlesource.com/platform/system/core/+/master/adb/SYNC.TXT#66
        //
        // When the file is transferred a sync request "DONE" is sent, where length is set
        // to the last modified time for the file. The server responds to this last
        // request (but not to chunk requests) with an "OKAY" sync response (length can
        // be ignored).
        let time: u32 = ((SystemTime::now().duration_since(SystemTime::UNIX_EPOCH))
            .unwrap()
            .as_secs()
            & 0xFFFF_FFFF) as u32;

        stream.write_all(SyncCommand::Done.code()).await?;
        write_length_little_endian(&mut stream, time as usize).await?;

        // Status.
        stream.read_exact(&mut buf[0..4]).await?;

        if buf.starts_with(SyncCommand::Okay.code()) {
            if enable_run_as {
                // Use cp -a to preserve the permissions set by push.
                let result = self
                    .execute_host_shell_command_as(
                        format!("cp -aR {} {}", dest1.display(), dest.display()).as_str(),
                        enable_run_as,
                    )
                    .await;
                if self.remove(dest1).await.is_err() {
                    warn!("Failed to remove {}", dest1.display());
                }
                result?;
            }
            Ok(())
        } else if buf.starts_with(SyncCommand::Fail.code()) {
            if enable_run_as && self.remove(dest1).await.is_err() {
                warn!("Failed to remove {}", dest1.display());
            }
            let n = buf.len().min(read_length_little_endian(&mut stream).await?);

            stream.read_exact(&mut buf[0..n]).await?;

            let message = std::str::from_utf8(&buf[0..n])
                .map(|s| format!("adb error: {}", s))
                .unwrap_or_else(|_| "adb error was not utf-8".into());

            Err(DeviceError::Adb(message))
        } else {
            if self.remove(dest1).await.is_err() {
                warn!("Failed to remove {}", dest1.display());
            }
            Err(DeviceError::Adb("FAIL (unknown)".to_owned()))
        }
    }

    pub async fn push_dir(&self, source: &Path, dest_dir: &UnixPath, mode: u32) -> Result<()> {
        self.push_dir_internal(source, dest_dir, mode, None).await
    }

    async fn push_dir_internal(
        &self,
        source: &Path,
        dest_dir: &UnixPath,
        mode: u32,
        progress_sender: Option<UnboundedSender<DirectoryTransferProgress>>,
    ) -> Result<()> {
        debug!("Pushing {} to {}", source.display(), dest_dir.display());

        // Calculate totals first
        let mut total_files = 0usize;
        let mut total_bytes = 0u64;
        let walker = WalkDir::new(source).follow_links(false).into_iter();
        for entry in walker {
            let entry = entry?;
            if entry.metadata()?.is_file() {
                total_files += 1;
                total_bytes += entry.metadata()?.len();
            }
        }

        // Send initial progress if progress reporting is enabled
        if let Some(sender) = &progress_sender {
            let _ = sender.send(DirectoryTransferProgress {
                directory_name: Some(dest_dir.display().to_string()),
                total_files,
                transferred_files: 0,
                total_bytes,
                transferred_bytes: 0,
                current_file: None,
                current_file_progress: FileTransferProgress {
                    total_bytes: 0,
                    transferred_bytes: 0,
                },
            });
        }

        let mut transferred_files = 0usize;
        let mut transferred_bytes = 0u64;

        let walker = WalkDir::new(source).follow_links(false).into_iter();
        for entry in walker {
            let entry = entry?;
            let path = entry.path();

            if !entry.metadata()?.is_file() {
                continue;
            }

            let file_size = entry.metadata()?.len();
            let mut file = BufReader::new(File::open(path).await?);

            let tail = path
                .strip_prefix(source)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

            let dest = append_components(dest_dir, tail)?;

            // Create a channel for file progress if directory progress is enabled
            let (file_sender, mut file_receiver): (
                Option<UnboundedSender<FileTransferProgress>>,
                Option<UnboundedReceiver<FileTransferProgress>>,
            ) = progress_sender
                .as_ref()
                .map(|_| tokio::sync::mpsc::unbounded_channel())
                .map(|(s, r)| (Some(s), Some(r)))
                .unwrap_or((None, None));

            // Send directory progress with current file
            if let Some(sender) = &progress_sender {
                let _ = sender.send(DirectoryTransferProgress {
                    directory_name: None,
                    total_files,
                    transferred_files,
                    total_bytes,
                    transferred_bytes,
                    current_file: Some(dest.display().to_string()),
                    current_file_progress: FileTransferProgress {
                        total_bytes: file_size,
                        transferred_bytes: 0,
                    },
                });

                // Spawn a task to handle file progress updates if progress reporting is enabled
                if let Some(mut receiver) = file_receiver.take() {
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        while let Some(file_progress) = receiver.recv().await {
                            let _ = sender.send(DirectoryTransferProgress {
                                directory_name: None,
                                total_files,
                                transferred_files,
                                total_bytes,
                                transferred_bytes: transferred_bytes
                                    + file_progress.transferred_bytes,
                                current_file: None,
                                current_file_progress: file_progress,
                            });
                        }
                    });
                }
            }

            // Push file with progress if enabled
            self.push_internal(&mut file, &dest, mode, Some(file_size), file_sender)
                .await?;

            transferred_files += 1;
            transferred_bytes += file_size;
        }

        Ok(())
    }

    pub async fn push_dir_with_progress(
        &self,
        source: &Path,
        dest_dir: &UnixPath,
        mode: u32,
        progress_sender: UnboundedSender<DirectoryTransferProgress>,
    ) -> Result<()> {
        self.push_dir_internal(source, dest_dir, mode, Some(progress_sender))
            .await
    }

    pub async fn pull_dir_with_progress(
        &self,
        src: &UnixPath,
        dest_dir: &Path,
        progress_sender: UnboundedSender<DirectoryTransferProgress>,
    ) -> Result<()> {
        self.pull_dir_internal(src, dest_dir, Some(progress_sender))
            .await
    }

    pub async fn remove(&self, path: &UnixPath) -> Result<()> {
        debug!("Deleting {}", path.display());

        self.execute_host_shell_command_as(
            &format!("rm -rf {}", path.display()),
            self.enable_run_as_for_path(path),
        )
        .await?;

        Ok(())
    }

    pub async fn tcpip(self, port: u16) -> Result<()> {
        debug!("Restarting adbd in TCP mode on port {}", port);

        let command = format!("tcpip:{}", port);
        self.execute_host_command(&command, false, true).await?;
        Ok(())
    }

    pub async fn usb(self) -> Result<()> {
        debug!("Restarting adbd in USB mode");

        let command = "usb:";
        self.execute_host_command(command, false, true).await?;
        Ok(())
    }

    pub async fn stat(&self, path: &UnixPath) -> Result<FileMetadata> {
        // Implement the ADB protocol to get file statistics from the device
        let mut stream = self.host.connect().await?;

        // Send "host:transport" command with device serial
        let message = encode_message(&format!("host:transport:{}", self.serial))?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "sync:" command to initialize file transfer
        let message = encode_message("sync:")?;
        stream.write_all(message.as_bytes()).await?;
        let _bytes = read_response(&mut stream, false, true).await?;

        // Send "STAT" command with path
        stream.write_all(SyncCommand::Stat.code()).await?;
        let args = format!("{}", path.display()).into_bytes();
        write_length_little_endian(&mut stream, args.len()).await?;
        stream.write_all(&args).await?;

        // Read response
        let mut response_code = [0u8; 4];
        stream.read_exact(&mut response_code).await?;

        if &response_code != SyncCommand::Stat.code() {
            return Err(DeviceError::Adb(format!(
                "Invalid response code: {:?}",
                std::str::from_utf8(&response_code)
            )));
        }

        // Read the 12 bytes containing mode (4), size (4), and time (4)
        let mut stat_data = [0u8; 12];
        stream.read_exact(&mut stat_data).await?;

        // Parse the data
        let mode = u32::from_le_bytes(stat_data[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(stat_data[4..8].try_into().unwrap());
        let time = u32::from_le_bytes(stat_data[8..12].try_into().unwrap());

        // Mode 0 indicates file not found
        if mode == 0 {
            return Err(DeviceError::Adb(
                "adb: stat failed: No such file or directory".to_owned(),
            ));
        }

        // Convert mode to UnixFileStatus
        let file_mode = match mode & 0xF000 {
            0x4000 => UnixFileStatus::Directory,
            0x2000 => UnixFileStatus::CharacterDevice,
            0x6000 => UnixFileStatus::BlockDevice,
            0x8000 => UnixFileStatus::RegularFile,
            0xA000 => UnixFileStatus::SymbolicLink,
            0xC000 => UnixFileStatus::Socket,
            _ => return Err(DeviceError::Adb(format!("Unknown file mode: {:#x}", mode))),
        };

        Ok(FileMetadata {
            path: path.display().to_string(),
            file_mode,
            size,
            modified_time: if time == 0 {
                None
            } else {
                Some(SystemTime::UNIX_EPOCH + StdDuration::from_secs(time as u64))
            },
            depth: None,
        })
    }

    pub async fn install_package(
        &self,
        apk_path: &Path,
        reinstall: bool,
        grant_runtime_permissions: bool,
    ) -> Result<()> {
        let apk_path = apk_path.to_path_buf();

        let base_name = apk_path
            .file_name()
            .ok_or(DeviceError::Adb("Invalid apk path".to_owned()))?
            .to_str()
            .ok_or(DeviceError::Adb("Invalid apk path".to_owned()))?;

        // push the apk to /data/local/tmp and run the "pm install" command
        let tmp_apk_path = UnixPathBuf::from("/data/local/tmp").join(base_name);
        let mut file = BufReader::new(File::open(apk_path).await?);
        self.push(&mut file, &tmp_apk_path, 0o644).await?;

        let mut command = "pm install".to_owned();
        if reinstall {
            command.push_str(" -r");
        }
        if grant_runtime_permissions {
            command.push_str(" -g");
        }
        command.push_str(&format!(" {}", tmp_apk_path.display()));
        let output = self.execute_host_shell_command(&command).await?;

        self.execute_host_shell_command(format!("rm {}", tmp_apk_path.display()).as_str())
            .await?;

        if !output.starts_with("Success") {
            return Err(DeviceError::PackageManagerError(output));
        }

        Ok(())
    }

    pub async fn install_package_with_progress(
        &self,
        apk_path: &Path,
        reinstall: bool,
        grant_runtime_permissions: bool,
        progress_sender: UnboundedSender<f32>,
    ) -> Result<()> {
        let apk_path = apk_path.to_path_buf();

        let base_name = apk_path
            .file_name()
            .ok_or(DeviceError::Adb("Invalid apk path".to_owned()))?
            .to_str()
            .ok_or(DeviceError::Adb("Invalid apk path".to_owned()))?;

        let file_metadata = std::fs::metadata(&apk_path)?;
        let file_size = file_metadata.len();

        let (push_sender, mut push_receiver) =
            tokio::sync::mpsc::unbounded_channel::<FileTransferProgress>();

        tokio::spawn({
            let progress_sender = progress_sender.clone();
            async move {
                while let Some(push_progress) = push_receiver.recv().await {
                    // Map push progress to install progress (up to 90%)
                    let _ = progress_sender
                        .send((push_progress.transferred_bytes as f32 / file_size as f32) * 0.9);
                }
            }
        });

        let tmp_apk_path = UnixPathBuf::from("/data/local/tmp").join(base_name);
        let mut file = BufReader::new(File::open(&apk_path).await?);
        self.push_with_progress(&mut file, &tmp_apk_path, 0o644, file_size, push_sender)
            .await?;

        let _ = progress_sender.send(0.9);

        let mut command = "pm install".to_owned();
        if reinstall {
            command.push_str(" -r");
        }
        if grant_runtime_permissions {
            command.push_str(" -g");
        }
        command.push_str(&format!(" {}", tmp_apk_path.display()));
        let output = self.execute_host_shell_command(&command).await?;

        self.execute_host_shell_command(format!("rm {}", tmp_apk_path.display()).as_str())
            .await?;

        if !output.starts_with("Success") {
            return Err(DeviceError::PackageManagerError(output));
        }

        let _ = progress_sender.send(1.0);

        Ok(())
    }

    pub async fn uninstall_package(&self, package: &str) -> Result<()> {
        let command = format!("pm uninstall {}", package);
        let output = self.execute_host_shell_command(&command).await?;
        if !output.starts_with("Success") {
            return Err(DeviceError::PackageManagerError(output));
        }

        Ok(())
    }

    pub async fn list_packages(&self, third_party: bool) -> Result<Vec<String>> {
        let command = if third_party {
            "pm list packages -3"
        } else {
            "pm list packages"
        };
        let output = self.execute_host_shell_command(command).await?;
        let mut packages = output
            .lines()
            .filter(|line| line.starts_with("package:"))
            .map(|line| {
                Ok(line
                    .split_once(':')
                    .ok_or(DeviceError::Adb(
                        "Failed to parse package list line".to_owned(),
                    ))?
                    .1
                    .to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        packages.sort();
        Ok(packages)
    }
}

pub(crate) fn append_components(
    base: &UnixPath,
    tail: &Path,
) -> std::result::Result<UnixPathBuf, io::Error> {
    let mut buf = base.to_path_buf();

    for component in tail.components() {
        if let Component::Normal(segment) = component {
            let utf8 = segment.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "Could not represent path segment as UTF-8",
                )
            })?;
            buf.push(utf8);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Unexpected path component".to_owned(),
            ));
        }
    }

    Ok(buf)
}

#[derive(Debug, Clone)]
pub struct FileTransferProgress {
    pub total_bytes: u64,
    pub transferred_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DirectoryTransferProgress {
    pub directory_name: Option<String>,
    pub total_files: usize,
    pub transferred_files: usize,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub current_file: Option<String>,
    pub current_file_progress: FileTransferProgress,
}
