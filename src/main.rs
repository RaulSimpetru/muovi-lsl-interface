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
//! The Muovi streams data in 18-sample packets at 2kHz, with each sample containing
//! 38 channels of 16-bit big-endian signed integers (1368 bytes per TCP packet).
//! Device control is performed via single-byte commands sent over the TCP connection.
//!
//! For complete device documentation and specifications, see:
//! <https://otbioelettronica.it/en/download/#55-171-wpfd-muovi>

use byteorder::{BigEndian, ByteOrder};
use chrono::Datelike;
use clap::Parser;
use lsl::ExPushable;
use socket2::{Domain, Socket, Type};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};
use thiserror::Error;

const CONVERSION_FACTOR: f32 = 286.1e-3; // 286.1 nV per bit = 0.2861 microvolts per bit (matches manufacturer's 0.000286 mV)

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
/// - Receives raw 16-bit ADC data in 18-sample packets (1368 bytes per TCP packet)
/// - Converts to microvolts using device-specific calibration
/// - Streams via LSL individually per sample with proper timestamps
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
    // Preallocated buffers for allocation-free streaming
    raw_samples: Vec<i16>,
    chunk_f32: Vec<Vec<f32>>,
    chunk_i16: Vec<Vec<i16>>,
    timestamps: Vec<f64>,
    timestamp_offsets: Vec<f64>,
}

impl Default for MuoviReader {
    fn default() -> Self {
        const N_SAMPLES: usize = 18;
        const N_CHANNELS: usize = 38;
        const SAMPLE_INTERVAL: f64 = 1.0 / 2000.0;

        // Precompute timestamp offsets (oldest sample has largest offset)
        let timestamp_offsets: Vec<f64> = (0..N_SAMPLES)
            .map(|i| (N_SAMPLES - 1 - i) as f64 * SAMPLE_INTERVAL)
            .collect();

        Self {
            host: "0.0.0.0".to_string(),
            port: 54321,
            connection_timeout__s: 30,
            data_timeout__s: 5,
            test_mode: false,
            apply_conversion: true,
            gain: 4,
            lsl_uuid: "muovi-180319".to_string(),
            // Preallocate all buffers
            raw_samples: vec![0i16; N_SAMPLES * N_CHANNELS],
            chunk_f32: vec![vec![0.0f32; N_CHANNELS]; N_SAMPLES],
            chunk_i16: vec![vec![0i16; N_CHANNELS]; N_SAMPLES],
            timestamps: vec![0.0f64; N_SAMPLES],
            timestamp_offsets,
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

    /// In-place conversion to microvolts - writes directly into preallocated output buffer.
    ///
    /// Applies the conversion factor (286.1 nV/bit) and gain compensation to the first 32 EMG channels.
    /// The last 6 channels (4 IMU quaternion + 2 diagnostic) are kept as raw values.
    /// This is optimized for high-performance streaming with zero allocations.
    #[inline]
    fn convert_data__f32_into(raw_data: &[i16], output: &mut [f32], gain_factor: f32) {
        for (i, (&x, out)) in raw_data.iter().zip(output.iter_mut()).enumerate() {
            *out = if i < 32 {
                // Convert first 32 EMG channels to microvolts
                x as f32 * CONVERSION_FACTOR * gain_factor
            } else {
                // Keep last 6 channels (IMU + diagnostic) as raw values
                x as f32
            };
        }
    }

    /// In-place copy of raw ADC values - writes directly into preallocated output buffer.
    ///
    /// Returns raw ADC values without conversion (for dimensionless output).
    /// This is optimized for high-performance streaming with zero allocations.
    #[inline]
    fn convert_data__i16_into(raw_data: &[i16], output: &mut [i16]) {
        output.copy_from_slice(raw_data);
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
            ch.append_child_value("label", &format!("Ch {}", i + 1));
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
        let imu_labels: [&'static str; 4] = ["Q W", "Q X", "Q Y", "Q Z"];
        for label in &imu_labels {
            let mut ch = chns.append_child("channel");
            ch.append_child_value("label", label);
            ch.append_child_value("unit", "dimensionless");
            ch.append_child_value("type", "IMU");
        }

        // Last 2 channels are diagnostic
        for i in 0..2 {
            let mut ch = chns.append_child("channel");
            ch.append_child_value("label", &format!("D {}", i + 1));
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

                    // Set TCP_NODELAY to disable Nagle's algorithm for low-latency streaming
                    stream.set_nodelay(true)?;

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
    /// Control byte format: [0][0][0][0][EMG][MODE1][MODE0][GO/STOP]
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
    /// 4. Continuously receives 18-sample packets (1368 bytes) and streams each sample individually via LSL
    /// 5. Handles timeouts and connection errors gracefully
    ///
    /// The loop continues until the device disconnects or an error occurs.
    /// Each sample receives its own timestamp based on the 2kHz sampling rate.
    /// Data is converted from raw ADC values to microvolts if conversion is enabled.
    ///
    /// Performance: This implementation uses preallocated buffers and in-place conversion
    /// to eliminate all heap allocations in the hot loop, ensuring consistent low-latency streaming.
    pub fn run(&mut self) -> Result<()> {
        let mut stream: TcpStream = self.wait_for_connection()?;

        let outlet: lsl::StreamOutlet =
            lsl::StreamOutlet::new(&self.create_stream_info()?, 18, 360)?;

        self.configure_device(&mut stream)?;

        // Buffer for 18 samples of 38 channels (16-bit each) = 1368 bytes per TCP packet
        let mut buffer: Vec<u8> = vec![0u8; 38 * 18 * 2];

        println!("Reading data... (Ctrl+C to stop)");

        #[cfg(feature = "timing")]
        {
            println!("Performance timing enabled");
        }

        // Timing statistics (only compiled when timing feature is enabled)
        #[cfg(feature = "timing")]
        let mut packet_count: u64 = 0;
        #[cfg(feature = "timing")]
        let mut total_read_us: f64 = 0.0;
        #[cfg(feature = "timing")]
        let mut total_convert_us: f64 = 0.0;
        #[cfg(feature = "timing")]
        let mut total_timestamp_us: f64 = 0.0;
        #[cfg(feature = "timing")]
        let mut total_data_conv_us: f64 = 0.0;
        #[cfg(feature = "timing")]
        let mut total_push_us: f64 = 0.0;
        #[cfg(feature = "timing")]
        let mut total_loop_us: f64 = 0.0;

        loop {
            #[cfg(feature = "timing")]
            let loop_start = Instant::now();

            match stream.read_exact(&mut buffer) {
                Ok(()) => {
                    #[cfg(feature = "timing")]
                    let read_time = Instant::now();

                    // Capture timestamp immediately after receiving the packet
                    let packet_timestamp: f64 = lsl::local_clock();

                    // Bulk convert bytes to i16 (big-endian) - single efficient operation
                    #[cfg(feature = "timing")]
                    let convert_start = Instant::now();
                    BigEndian::read_i16_into(&buffer, &mut self.raw_samples);
                    #[cfg(feature = "timing")]
                    let convert_time = Instant::now();

                    // Fill timestamps using precomputed offsets
                    #[cfg(feature = "timing")]
                    let timestamp_start = Instant::now();
                    for (i, &offset) in self.timestamp_offsets.iter().enumerate() {
                        self.timestamps[i] = packet_timestamp - offset;
                    }
                    #[cfg(feature = "timing")]
                    let timestamp_time = Instant::now();

                    // Convert and push data using preallocated buffers (no allocations)
                    #[cfg(feature = "timing")]
                    let data_conv_start = Instant::now();
                    if self.apply_conversion {
                        let gain_factor = self.get_gain_factor();
                        for (i, raw_sample) in self.raw_samples.chunks_exact(38).enumerate() {
                            Self::convert_data__f32_into(
                                raw_sample,
                                &mut self.chunk_f32[i],
                                gain_factor,
                            );
                        }
                        #[cfg(feature = "timing")]
                        let data_conv_time = Instant::now();

                        #[cfg(feature = "timing")]
                        let push_start = Instant::now();
                        outlet.push_chunk_stamped_ex(&self.chunk_f32, &self.timestamps, true)?;
                        #[cfg(feature = "timing")]
                        let push_time = Instant::now();

                        #[cfg(feature = "timing")]
                        {
                            let data_conv_elapsed = data_conv_time.duration_since(data_conv_start).as_micros() as f64;
                            let push_elapsed = push_time.duration_since(push_start).as_micros() as f64;
                            total_data_conv_us += data_conv_elapsed;
                            total_push_us += push_elapsed;
                        }
                    } else {
                        for (i, raw_sample) in self.raw_samples.chunks_exact(38).enumerate() {
                            Self::convert_data__i16_into(raw_sample, &mut self.chunk_i16[i]);
                        }
                        #[cfg(feature = "timing")]
                        let data_conv_time = Instant::now();

                        #[cfg(feature = "timing")]
                        let push_start = Instant::now();
                        outlet.push_chunk_stamped_ex(&self.chunk_i16, &self.timestamps, true)?;
                        #[cfg(feature = "timing")]
                        let push_time = Instant::now();

                        #[cfg(feature = "timing")]
                        {
                            let data_conv_elapsed = data_conv_time.duration_since(data_conv_start).as_micros() as f64;
                            let push_elapsed = push_time.duration_since(push_start).as_micros() as f64;
                            total_data_conv_us += data_conv_elapsed;
                            total_push_us += push_elapsed;
                        }
                    }

                    #[cfg(feature = "timing")]
                    {
                        let loop_end = Instant::now();

                        let read_elapsed = read_time.duration_since(loop_start).as_micros() as f64;
                        let convert_elapsed = convert_time.duration_since(convert_start).as_micros() as f64;
                        let timestamp_elapsed = timestamp_time.duration_since(timestamp_start).as_micros() as f64;
                        let loop_elapsed = loop_end.duration_since(loop_start).as_micros() as f64;

                        total_read_us += read_elapsed;
                        total_convert_us += convert_elapsed;
                        total_timestamp_us += timestamp_elapsed;
                        total_loop_us += loop_elapsed;
                        packet_count += 1;

                        // Print statistics every 1000 packets (every ~9 seconds at 2kHz)
                        if packet_count % 1000 == 0 {
                            println!("\n=== Performance Stats (avg over {} packets) ===", packet_count);
                            println!("Network read:\t\t{:.2} µs", total_read_us / packet_count as f64);
                            println!("Byte->i16 conversion:\t{:.2} µs", total_convert_us / packet_count as f64);
                            println!("Timestamp calc:\t\t{:.2} µs", total_timestamp_us / packet_count as f64);
                            println!("Data conversion:\t{:.2} µs", total_data_conv_us / packet_count as f64);
                            println!("LSL push:\t\t{:.2} µs", total_push_us / packet_count as f64);
                            println!("Total loop time:\t{:.2} µs", total_loop_us / packet_count as f64);
                            println!("Overhead:\t\t{:.2} µs", (total_loop_us - total_read_us - total_convert_us - total_timestamp_us - total_data_conv_us - total_push_us) / packet_count as f64);
                            println!("===============================================\n");
                        }
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

    println!(
        "muovi-lsl-interface Copyright (C) {} Raul C. Sîmpetru",
        copyright_year
    );
    println!("This program comes with ABSOLUTELY NO WARRANTY; for details see");
    println!("https://www.gnu.org/licenses/gpl-3.0.html#license-text.");
    println!("This is free software, and you are welcome to redistribute it");
    println!(
        "under certain conditions; for details see https://www.gnu.org/licenses/gpl-3.0.html#license-text."
    );
    println!();

    let args = Args::parse();

    // Validate gain
    if args.gain != 4 && args.gain != 8 {
        eprintln!("Error: Gain must be 4 or 8");
        std::process::exit(1);
    }

    let mut reader = MuoviReader::new()
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
