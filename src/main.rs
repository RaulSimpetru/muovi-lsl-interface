//! Muovi EMG device TCP server and LSL streamer
//!
//! This module implements a TCP server that connects to OT Bioelettronica Muovi devices
//! and streams 38-channel EMG data via Lab Streaming Layer (LSL). The Muovi is a wireless
//! EMG acquisition system with 32 EMG channels, 4 IMU quaternion channels, and 2 diagnostic channels.
//!
//! ## Features
//! - TCP server with configurable host/port and timeouts
//! - Support for both EMG modes (gain 4 or 8) and test mode (ramp signals)
//! - Real-time data conversion from raw ADC values to microvolts
//! - LSL streaming with proper channel metadata
//! - Builder pattern API for easy configuration
//!
//! ## Device Protocol
//! The Muovi streams data in 18-sample chunks at 2kHz, with each sample containing
//! 38 channels of 16-bit big-endian signed integers. Device control is performed
//! via single-byte commands sent over the TCP connection.
//!
//! For complete device documentation and specifications, see:
//! <https://otbioelettronica.it/en/download/#55-171-wpfd-muovi>

use byteorder::{BigEndian, ReadBytesExt};
use chrono::Datelike;
use clap::Parser;
use lsl::ExPushable;
use socket2::{Domain, Socket, Type};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};
use thiserror::Error;

const CONVERSION_FACTOR: f32 = 286.1e-9 / 1000.0; // 286.1 nV per bit to microvolts

#[derive(Error, Debug)]
pub enum MuoviError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("LSL error: {0}")]
    Lsl(#[from] lsl::Error),
    #[error("Connection timeout: No connection received within {0} seconds")]
    ConnectionTimeout(u64),
    #[error("Data timeout: No data received within {0} seconds")]
    DataTimeout(u64),
}

type Result<T> = std::result::Result<T, MuoviError>;

#[derive(Parser)]
#[command(name = "muovi-lsl-interface")]
#[command(about = "Muovi LSL Interface - streams EMG data via Lab Streaming Layer")]
struct Args {
    #[arg(long, default_value = "0.0.0.0", help = "Host to bind to")]
    host: String,

    #[arg(long, default_value = "54321", help = "Port to listen on")]
    port: u16,

    #[arg(long, default_value = "30", help = "Connection timeout in seconds")]
    connection_timeout: u64,

    #[arg(long, default_value = "5", help = "Data timeout in seconds")]
    data_timeout: u64,

    #[arg(long, help = "Use test mode (generates ramp signals)")]
    test_mode: bool,

    #[arg(
        long,
        default_value = "4",
        help = "Amplifier gain for EMG mode (4 or 8)"
    )]
    gain: u8,

    #[arg(long, help = "Stream raw ADC values instead of microvolts")]
    no_conversion: bool,

    #[arg(long, default_value = "muovi-180319", help = "LSL stream source ID")]
    lsl_uuid: String,
}

/// A TCP server that connects to Muovi EMG devices and streams data via LSL.
///
/// The `MuoviReader` handles the complete pipeline from device connection to LSL streaming:
/// - Establishes TCP connection with configurable timeouts
/// - Configures the device for EMG acquisition or test mode
/// - Receives raw 16-bit ADC data in 18-sample chunks
/// - Converts to microvolts using device-specific calibration
/// - Streams via LSL with proper channel metadata
///
/// # Example
/// ```no_run
/// let reader = MuoviReader::new()
///     .host("192.168.1.100")
///     .port(54321)
///     .gain(4)
///     .connection_timeout(30);
/// reader.run()?;
/// ```
#[allow(non_snake_case)]
pub struct MuoviReader {
    host: String,
    port: u16,
    connection_timeout__s: u64,
    data_timeout__s: u64,
    test_mode: bool,
    apply_conversion: bool,
    gain: u8,
    lsl_uuid: String,
}

impl Default for MuoviReader {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 54321,
            connection_timeout__s: 30,
            data_timeout__s: 5,
            test_mode: false,
            apply_conversion: true,
            gain: 4,
            lsl_uuid: "muovi-180319".to_string(),
        }
    }
}

#[allow(non_snake_case)]
impl MuoviReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn connection_timeout(mut self, timeout_s: u64) -> Self {
        self.connection_timeout__s = timeout_s;
        self
    }

    pub fn data_timeout(mut self, timeout_s: u64) -> Self {
        self.data_timeout__s = timeout_s;
        self
    }

    pub fn test_mode(mut self, test_mode: bool) -> Self {
        self.test_mode = test_mode;
        self
    }

    pub fn apply_conversion(mut self, apply_conversion: bool) -> Self {
        self.apply_conversion = apply_conversion;
        self
    }

    pub fn gain(mut self, gain: u8) -> Self {
        self.gain = gain;
        self
    }

    /// Sets the LSL stream source ID for identification.
    pub fn lsl_uuid(mut self, lsl_uuid: &str) -> Self {
        self.lsl_uuid = lsl_uuid.to_string();
        self
    }

    /// Returns the gain compensation factor based on device settings.
    ///
    /// The Muovi requires a 2x gain compensation when using gain=4 in EMG mode.
    /// No compensation is applied in test mode or when using gain=8.
    fn get_gain_factor(&self) -> f32 {
        if !self.test_mode && self.gain == 4 {
            2.0
        } else {
            1.0
        }
    }

    /// Converts raw 16-bit ADC values to microvolts using device calibration.
    ///
    /// Applies the conversion factor (286.1 nV/bit) and gain compensation.
    fn convert_data__f32(&self, raw_data: &[i16]) -> Vec<f32> {
        raw_data
            .iter()
            .map(|&x| x as f32 * CONVERSION_FACTOR * self.get_gain_factor())
            .collect()
    }

    /// Returns raw ADC values without conversion (for dimensionless output).
    fn convert_data__i16(&self, raw_data: &[i16]) -> Vec<i16> {
        raw_data.to_vec()
    }

    /// Creates LSL stream info with proper channel metadata.
    ///
    /// Sets up 38 channels: 32 EMG, 4 IMU quaternion, 2 diagnostic.
    /// Channel units depend on conversion setting (microvolts vs dimensionless).
    fn create_stream_info(&self) -> Result<lsl::StreamInfo> {
        let mut info: lsl::StreamInfo = lsl::StreamInfo::new(
            "MUOVI",
            "MUOVI_DATA",
            38,
            2000.0,
            if self.apply_conversion {
                lsl::ChannelFormat::Float32
            } else {
                lsl::ChannelFormat::Int16
            },
            &self.lsl_uuid,
        )?;

        // Add manufacturer and device information
        let mut desc: lsl::XMLElement = info.desc();
        desc.append_child_value("manufacturer", "OTBioelettronica");
        desc.append_child_value("device", "Muovi");
        desc.append_child_value(
            "conversion_factor",
            &(CONVERSION_FACTOR * self.get_gain_factor()).to_string(),
        );

        // Add channel information
        let mut chns: lsl::XMLElement = desc.append_child("channels");

        // First 32 channels are EMG
        for i in 0..32 {
            let mut ch: lsl::XMLElement = chns.append_child("channel");
            ch.append_child_value("label", &format!("EMG_{}", i + 1));
            ch.append_child_value(
                "unit",
                if self.apply_conversion {
                    "microvolts"
                } else {
                    "dimensionless"
                },
            );
            ch.append_child_value("type", "EMG");
        }

        // Channels 32-36 are IMU quaternion values
        let imu_labels: [&'static str; 4] = ["QUAT_W", "QUAT_X", "QUAT_Y", "QUAT_Z"];
        for label in &imu_labels {
            let mut ch = chns.append_child("channel");
            ch.append_child_value("label", label);
            ch.append_child_value("unit", "dimensionless");
            ch.append_child_value("type", "IMU");
        }

        // Last 2 channels are diagnostic
        for i in 0..2 {
            let mut ch = chns.append_child("channel");
            ch.append_child_value("label", &format!("DIAG_{}", i + 1));
            ch.append_child_value("unit", "dimensionless");
            ch.append_child_value("type", "DIAGNOSTIC");
        }

        Ok(info)
    }

    /// Waits for a TCP connection from the Muovi device.
    ///
    /// Creates a non-blocking TCP listener with SO_REUSEADDR and waits for
    /// connection within the configured timeout. Sets appropriate timeouts
    /// on the resulting stream for data reception.
    fn wait_for_connection(&self) -> Result<TcpStream> {
        let address: SocketAddr = format!("{}:{}", self.host, self.port)
            .parse::<SocketAddr>()
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid address")
            })?;

        // Create socket with SO_REUSEADDR
        let socket: Socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        socket.set_reuse_address(true)?;
        socket.bind(&address.into())?;
        socket.listen(1)?;

        let listener: TcpListener = socket.into();
        listener.set_nonblocking(true)?;

        println!(
            "Waiting for Muovi connection on {}:{} (timeout: {}s)",
            self.host, self.port, self.connection_timeout__s
        );

        let start_time: Instant = Instant::now();

        loop {
            if start_time.elapsed().as_secs() > self.connection_timeout__s {
                return Err(MuoviError::ConnectionTimeout(self.connection_timeout__s));
            }

            match listener.accept() {
                Ok((stream, addr)) => {
                    println!("Muovi connected from {}", addr);

                    // Set stream to blocking mode for data reception
                    stream.set_nonblocking(false)?;

                    // Set timeouts for data reception
                    stream.set_read_timeout(Some(Duration::from_secs(self.data_timeout__s)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(self.data_timeout__s)))?;

                    return Ok(stream);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                #[cfg(windows)]
                Err(ref e) if e.raw_os_error() == Some(10035) => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => return Err(MuoviError::Io(e)),
            }
        }
    }

    /// Sends device configuration command to start data acquisition.
    ///
    /// Control byte format: [0][0][0][EMG][MODE1][MODE0][GO][STOP]
    /// - EMG=1: Enable EMG channels
    /// - MODE: 00=gain8, 01=gain4, 11=test_ramps
    /// - GO=1: Start streaming
    fn configure_device(&self, stream: &mut TcpStream) -> Result<()> {
        let control_byte: u8 = if self.test_mode {
            // Test mode: EMG=1, MODE=11 (test ramps), GO=1
            0b00001111u8
        } else {
            // Monopolar: EMG=1, MODE=00|01 (gain 8|4), GO=1
            if self.gain == 8 {
                0b00001001u8
            } else {
                0b00001011u8
            }
        };

        stream.write_all(&[control_byte])?;
        Ok(())
    }

    /// Runs the main acquisition loop: connect, configure, and stream data.
    ///
    /// This method:
    /// 1. Waits for TCP connection from the Muovi device
    /// 2. Creates LSL outlet with proper channel metadata
    /// 3. Configures the device for data acquisition
    /// 4. Continuously receives 18-sample chunks and streams via LSL
    /// 5. Handles timeouts and connection errors gracefully
    ///
    /// The loop continues until the device disconnects or an error occurs.
    /// Data is converted from raw ADC values to microvolts if conversion is enabled.
    pub fn run(&self) -> Result<()> {
        let mut stream: TcpStream = self.wait_for_connection()?;

        let outlet: lsl::StreamOutlet =
            lsl::StreamOutlet::new(&self.create_stream_info()?, 18, 360)?;

        self.configure_device(&mut stream)?;

        // Buffer for 18 samples of 38 channels (16-bit each)
        let mut buffer: Vec<u8> = vec![0u8; 38 * 18 * 2];

        println!("Reading data... (Ctrl+C to stop)");

        loop {
            match stream.read_exact(&mut buffer) {
                Ok(()) => {
                    let timestamp: f64 = lsl::local_clock();

                    // Convert bytes to 16-bit signed integers (big-endian)
                    let mut raw_samples: Vec<i16> = Vec::with_capacity(18 * 38);
                    let mut cursor: std::io::Cursor<&Vec<u8>> = std::io::Cursor::new(&buffer);

                    for _ in 0..(18 * 38) {
                        raw_samples.push(cursor.read_i16::<BigEndian>()?);
                    }

                    // Reshape and convert data with individual timestamps
                    // At 2kHz sampling rate, each sample is 0.5ms apart (1/2000 = 0.0005 seconds)
                    let sample_interval = 1.0 / 2000.0;

                    macro_rules! process_chunk {
                        ($value_type:ty, $convert_fn:ident) => {{
                            let mut chunk: Vec<Vec<$value_type>> = Vec::with_capacity(18);
                            let mut timestamps: Vec<f64> = Vec::with_capacity(18);
                            for sample_idx in 0..18 {
                                let sample_start = sample_idx * 38;
                                let sample_end = sample_start + 38;
                                let sample_data = &raw_samples[sample_start..sample_end];
                                chunk.push(self.$convert_fn(sample_data));
                                // Calculate timestamp for each sample (oldest sample gets earliest timestamp)
                                timestamps.push(timestamp - (17 - sample_idx) as f64 * sample_interval);
                            }
                            outlet.push_chunk_stamped_ex(&chunk, &timestamps, true)?;
                        }};
                    }

                    if self.apply_conversion {
                        process_chunk!(f32, convert_data__f32);
                    } else {
                        process_chunk!(i16, convert_data__i16);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(MuoviError::DataTimeout(self.data_timeout__s));
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    println!("Connection closed by Muovi");
                    break;
                }
                Err(e) => return Err(MuoviError::Io(e)),
            }
        }

        // Stop streaming - set GO/STOP bit to 0
        stream.write_all(&[0b00001010u8])?;

        Ok(())
    }
}

fn main() -> Result<()> {
    // Display GPL license notice
    let current_year = chrono::Utc::now().year();
    let copyright_year = if current_year == 2025 {
        "2025".to_string()
    } else {
        format!("2025-{}", current_year)
    };

    println!("muovi-lsl-interface Copyright (C) {} Raul C. Sîmpetru", copyright_year);
    println!("This program comes with ABSOLUTELY NO WARRANTY; for details see");
    println!("https://www.gnu.org/licenses/gpl-3.0.html#license-text.");
    println!("This is free software, and you are welcome to redistribute it");
    println!("under certain conditions; for details see https://www.gnu.org/licenses/gpl-3.0.html#license-text.");
    println!();

    let args = Args::parse();

    // Validate gain
    if args.gain != 4 && args.gain != 8 {
        eprintln!("Error: Gain must be 4 or 8");
        std::process::exit(1);
    }

    let reader = MuoviReader::new()
        .host(&args.host)
        .port(args.port)
        .connection_timeout(args.connection_timeout)
        .data_timeout(args.data_timeout)
        .test_mode(args.test_mode)
        .apply_conversion(!args.no_conversion)
        .gain(args.gain)
        .lsl_uuid(&args.lsl_uuid);

    match reader.run() {
        Ok(()) => println!("Connection closed"),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
