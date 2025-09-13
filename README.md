# Muovi LSL Interface

A Rust-based TCP server that connects to OT Bioelettronica Muovi EMG devices and streams real-time data via Lab Streaming Layer (LSL) for research applications.

Link to device: [OT Bioelettronica Muovi](https://otbioelettronica.it/en/muovi/)

## Features

- **Real-time EMG streaming**: 38-channel data at 2kHz sampling rate
- **Multiple acquisition modes**: EMG with configurable gain (4x/8x) or test mode
- **LSL integration**: Seamless streaming with proper channel metadata
- **Robust connectivity**: Configurable timeouts and error handling
- **Cross-platform**: Runs on Linux, macOS, and Windows

## Channel Configuration

- **32 EMG channels**: High-quality EMG activity recording
- **4 IMU channels**: Quaternion-based orientation tracking (QUAT_W, QUAT_X, QUAT_Y, QUAT_Z)
- **2 Diagnostic channels**: System monitoring and troubleshooting

## Installation

### Prerequisites

- Rust toolchain (install from [rustup.rs](https://rustup.rs/))
- Lab Streaming Layer (LSL) library

### Build from source

```bash
git clone <repository-url>
cd muovi-lsl-interface
cargo build --release
```

The binary will be available at `target/release/muovi-streamer`.

## Usage

### Basic usage

```bash
./muovi-lsl-interface
```

### Command-line options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `--host` | String | `0.0.0.0` | Host address to bind TCP server |
| `--port` | u16 | `54321` | Port number for TCP connection |
| `--connection-timeout` | u64 | `30` | Connection timeout in seconds |
| `--data-timeout` | u64 | `5` | Data reception timeout in seconds |
| `--gain` | u8 | `4` | Amplifier gain for EMG mode (4 or 8) |
| `--test-mode` | Flag | `false` | Enable test mode (generates ramp signals) |
| `--no-conversion` | Flag | `false` | Stream raw ADC values instead of microvolts |
| `--lsl-uuid` | String | `muovi-180319` | LSL stream source ID for identification |

### Example configurations

**Standard EMG recording:**

```bash
./muovi-lsl-interface
```

**Test mode for development:**

```bash
./muovi-lsl-interface --test-mode
```

**Raw ADC values for custom processing:**

```bash
./muovi-lsl-interface --no-conversion
```

## LSL Integration

The streamer creates an LSL outlet with the following properties:

- **Stream name**: `MUOVI`
- **Stream type**: `MUOVI_DATA`
- **Channel count**: 38
- **Sample rate**: 2000.0 Hz
- **Channel format**: Float32 (converted) or Int16 (raw)
- **Source ID**: `muovi-180319` (configurable with `--lsl-uuid`)

### Channel Metadata

Each channel includes proper metadata for analysis software:

- **EMG channels**: Type="EMG", Units="microvolts" (or "dimensionless")
- **IMU channels**: Type="IMU", Units="dimensionless"
- **Diagnostic channels**: Type="DIAGNOSTIC", Units="dimensionless"

## Error Handling

The application handles various error conditions:

- **Connection timeout**: No device connection within specified time
- **Data timeout**: No data received from connected device
- **IO errors**: Network or communication failures
- **LSL errors**: Streaming layer issues

## Contributing

This project is developed for research purposes. For issues or contributions, please contact the maintainer.

## License

This project is licensed under the GPL-3.0 License - see the [LICENSE](LICENSE.md) file for details.

## Author

**Raul C. Sîmpetru**  
Email: <raul.simpetru@fau.de>  
Affiliation: Friedrich-Alexander-Universität Erlangen-Nürnberg

## Acknowledgments

- OT Bioelettronica for the Muovi EMG system
- Lab Streaming Layer (LSL) project
- Rust community for excellent tooling
