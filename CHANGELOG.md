# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2025-09-22

### Fixed

- **Individual sample timestamps**: Fixed timestamp assignment to properly reflect the temporal structure of the data
  - Each of the 18 samples in a chunk now receives its correct acquisition timestamp
  - Samples are spaced 0.5ms apart (1/2000 Hz) with the oldest sample receiving the earliest timestamp
  - Replaced single timestamp per chunk with individual timestamps for accurate temporal representation
- **Optimized data transmission**: Switched from individual sample pushing to batch chunk transmission
  - Uses `push_chunk_stamped_ex()` for more efficient network transmission
  - Maintains proper timestamp precision while improving performance
  - Reduces network overhead and improves real-time streaming characteristics

### Technical Details

- Sample timing calculation: `timestamp - (17 - sample_idx) * 0.0005` seconds
- Chunk-based transmission preserves temporal accuracy while optimizing network efficiency
- Compatible with existing LSL analysis tools expecting proper temporal sampling

## [1.0.0] - 2025-09-13

### Added

- **Initial release** of Muovi LSL Streamer for research applications
- **TCP server implementation** for OT Bioelettronica Muovi EMG device communication
- **Real-time data streaming** via Lab Streaming Layer (LSL) protocol
- **Multi-mode support**:
  - EMG acquisition mode with configurable amplifier gain (4x or 8x)
  - Test mode generating ramp signals for development and testing
- **Data processing pipeline**:
  - Raw 16-bit ADC value reception in 18-sample chunks at 2kHz
  - Automatic conversion to microvolts using device-specific calibration (286.1 nV/bit)
  - Gain compensation for accurate signal amplitude
- **38-channel configuration**:
  - 32 EMG channels for muscle activity recording
  - 4 IMU quaternion channels for orientation tracking
  - 2 diagnostic channels for system monitoring
- **Robust connection handling**:
  - Configurable TCP host and port settings
  - Connection timeout management
  - Data reception timeout with automatic error handling
  - Graceful device disconnection detection
- **Comprehensive CLI interface**:
  - Command-line argument parsing with clap
  - Flexible configuration options for research setups
  - Help documentation and usage examples
- **Professional documentation**:
  - Complete API documentation with device protocol details
  - Integration with official OT Bioelettronica documentation
  - Code examples and usage patterns
- **Error handling**:
  - Structured error types for different failure modes
  - Informative error messages for troubleshooting
  - Proper cleanup and resource management
