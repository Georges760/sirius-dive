mod ble;
mod parser;
mod protocol;
mod types;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use btleplug::api::Peripheral as _;
use clap::{Parser, Subcommand, ValueEnum};

use crate::types::*;

#[derive(Parser)]
#[command(name = "sirius-dive")]
#[command(about = "Extract dive logs from Mares Sirius dive computer via BLE")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan for Mares BLE devices and enumerate their GATT services
    Scan {
        /// Scan duration in seconds
        #[arg(short, long, default_value = "10")]
        timeout: u64,

        /// Connect to the first found device and enumerate GATT
        #[arg(short, long)]
        enumerate: bool,
    },

    /// Connect and query device info (model, serial, firmware)
    Info {
        /// BLE device address (e.g. "AA:BB:CC:DD:EE:FF"). If omitted, connects to first Mares device found.
        #[arg(short, long)]
        address: Option<String>,
    },

    /// Download dive logs from the device
    Download {
        /// BLE device address. If omitted, connects to first Mares device found.
        #[arg(short, long)]
        address: Option<String>,

        /// Output file path
        #[arg(short, long, default_value = "dives.json")]
        output: PathBuf,

        /// Output format
        #[arg(short, long, default_value = "json")]
        format: OutputFormat,

        /// Save raw dive data for debugging
        #[arg(long)]
        save_raw: Option<PathBuf>,
    },

    /// Raw protocol debug: test ECOP SDO communication
    Debug {
        /// BLE device address. If omitted, connects to first Mares device found.
        #[arg(short, long)]
        address: Option<String>,
    },

    /// Parse previously downloaded raw dive data (offline, no BLE needed)
    Parse {
        /// Directory containing raw dive data (dive_NNN_header.bin / dive_NNN_profile.bin)
        #[arg(short, long)]
        raw_dir: PathBuf,

        /// Output file path
        #[arg(short, long, default_value = "dives.json")]
        output: PathBuf,

        /// Output format
        #[arg(short, long, default_value = "json")]
        format: OutputFormat,
    },
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Csv,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan { timeout, enumerate } => cmd_scan(timeout, enumerate).await,
        Commands::Info { address } => cmd_info(address).await,
        Commands::Download {
            address,
            output,
            format,
            save_raw,
        } => cmd_download(address, output, format, save_raw).await,
        Commands::Debug { address } => cmd_debug(address).await,
        Commands::Parse {
            raw_dir,
            output,
            format,
        } => cmd_parse(raw_dir, output, format),
    }
}

// ── Scan ──

async fn cmd_scan(timeout_secs: u64, enumerate: bool) -> Result<()> {
    let adapter = ble::get_adapter().await?;

    eprintln!("Scanning for Mares BLE devices ({timeout_secs}s)...");
    let devices = ble::scan_for_devices(&adapter, Duration::from_secs(timeout_secs)).await?;

    if devices.is_empty() {
        eprintln!("No Mares devices found. Make sure the dive computer is in Bluetooth mode.");
        return Ok(());
    }

    println!("\nFound {} device(s):", devices.len());
    for (i, dev) in devices.iter().enumerate() {
        println!(
            "  [{}] {} - {} (RSSI: {})",
            i,
            dev.name,
            dev.address,
            dev.rssi
                .map(|r| format!("{r} dBm"))
                .unwrap_or_else(|| "?".into())
        );
    }

    if enumerate {
        let dev = &devices[0];
        eprintln!("\nConnecting to {}...", dev.name);
        dev.peripheral.connect().await?;

        let services = ble::enumerate_gatt(&dev.peripheral).await?;
        println!("\nGATT Profile for {}:", dev.name);
        for svc in &services {
            println!("  Service: {}", svc.uuid);
            for c in &svc.characteristics {
                println!("    Characteristic: {} [{}]", c.uuid, c.properties);
            }
        }

        dev.peripheral.disconnect().await?;
    }

    Ok(())
}

// ── Debug ──

async fn cmd_debug(address: Option<String>) -> Result<()> {
    let adapter = ble::get_adapter().await?;
    let peripheral = find_device(&adapter, address.as_deref()).await?;
    let mut conn = ble::connect(&peripheral, None, None).await?;

    eprintln!("=== ECOP SDO Protocol Test ===\n");

    // Step 1: CMD_VERSION
    eprintln!("--- Step 1: CMD_VERSION ---");
    let info = protocol::get_device_info(&mut conn).await?;
    eprintln!("  Model: {}", info.model_name);

    // Step 2: Read device info via ECOP
    eprintln!("\n--- Step 2: ECOP reads (device objects 0x2000) ---");

    eprintln!("  Reading 0x2000 sub 4 (PCB number)...");
    match protocol::ecop_read(&mut conn, 0x2000, 4).await {
        Ok(data) => {
            let s = String::from_utf8_lossy(&data);
            eprintln!("    Data ({} bytes): {:?}", data.len(), s.trim_end_matches('\0'));
            eprintln!("    Hex: [{}]", protocol::hex_dump(&data));
        }
        Err(e) => eprintln!("    Error: {e}"),
    }

    eprintln!("  Reading 0x2000 sub 8 (warranty)...");
    match protocol::ecop_read(&mut conn, 0x2000, 8).await {
        Ok(data) => eprintln!("    Data ({} bytes): [{}]", data.len(), protocol::hex_dump(&data)),
        Err(e) => eprintln!("    Error: {e}"),
    }

    eprintln!("  Reading 0x2008 sub 1...");
    match protocol::ecop_read(&mut conn, 0x2008, 1).await {
        Ok(data) => eprintln!("    Data ({} bytes): [{}]", data.len(), protocol::hex_dump(&data)),
        Err(e) => eprintln!("    Error: {e}"),
    }

    eprintln!("  Reading 0x2006 sub 12 (dive mode name)...");
    match protocol::ecop_read(&mut conn, 0x2006, 12).await {
        Ok(data) => {
            let s = String::from_utf8_lossy(&data);
            eprintln!("    Data: {:?}", s.trim_end_matches('\0'));
        }
        Err(e) => eprintln!("    Error: {e}"),
    }

    // Step 3: Set datetime
    eprintln!("\n--- Step 3: Set datetime (B0) ---");
    match protocol::set_datetime(&mut conn).await {
        Ok(()) => eprintln!("  DateTime set OK"),
        Err(e) => eprintln!("  DateTime failed: {e}"),
    }

    // Step 4: Count dives
    eprintln!("\n--- Step 4: Count dive objects ---");
    match protocol::count_dives(&mut conn).await {
        Ok(count) => eprintln!("  Found {count} dive object(s)"),
        Err(e) => eprintln!("  Count failed: {e}"),
    }

    // Step 5: Read first dive header
    eprintln!("\n--- Step 5: Read first dive header (0x3000 sub 4) ---");
    match protocol::read_dive_header(&mut conn, 0).await {
        Ok(data) => {
            eprintln!("  Header ({} bytes)", data.len());
            if data.len() >= 4 {
                let obj_type = data[0];
                eprintln!("  Object type: {} ({})", obj_type, match obj_type {
                    1 => "SCUBA",
                    2 => "FREEDIVE",
                    3 => "GAUGE",
                    _ => "unknown",
                });
            }
            if data.len() >= 12 {
                // Bytes 4-7 should be a timestamp
                let ts = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                if let Some(dt) = chrono::DateTime::from_timestamp(ts as i64, 0) {
                    eprintln!("  Timestamp: {} ({})", ts, dt.format("%Y-%m-%d %H:%M:%S UTC"));
                } else {
                    eprintln!("  Timestamp raw: {}", ts);
                }
            }
            // Print first 40 bytes hex
            let show = data.len().min(40);
            eprintln!("  First {} bytes: [{}]", show, protocol::hex_dump(&data[..show]));
        }
        Err(e) => eprintln!("  Error: {e}"),
    }

    // Step 6: Read first dive profile (if available)
    eprintln!("\n--- Step 6: Read first dive profile (0x3000 sub 3) ---");
    match protocol::read_dive_profile(&mut conn, 0).await {
        Ok(data) => {
            eprintln!("  Profile ({} bytes)", data.len());
            let show = data.len().min(60);
            eprintln!("  First {} bytes: [{}]", show, protocol::hex_dump(&data[..show]));

            // Look for record markers
            let markers = ["DSTR", "TISS", "DPRS", "AIRS"];
            for marker in &markers {
                let count = data
                    .windows(4)
                    .filter(|w| *w == marker.as_bytes())
                    .count();
                if count > 0 {
                    eprintln!("  {} records: {}", marker, count);
                }
            }
        }
        Err(e) => eprintln!("  Error: {e}"),
    }

    conn.disconnect().await?;
    eprintln!("\nDone.");
    Ok(())
}

// ── Info ──

async fn cmd_info(address: Option<String>) -> Result<()> {
    let adapter = ble::get_adapter().await?;
    let peripheral = find_device(&adapter, address.as_deref()).await?;
    let mut conn = ble::connect(&peripheral, None, None).await?;

    let info = protocol::get_device_info(&mut conn).await?;

    // Read PCB number via ECOP
    let pcb = match protocol::read_pcb_number(&mut conn).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: could not read PCB number: {e}");
            String::from("unknown")
        }
    };

    // Count dives
    let dive_count = match protocol::count_dives(&mut conn).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Warning: could not count dives: {e}");
            0
        }
    };

    println!("Device Info:");
    println!("  Model:      {} (0x{:02X})", info.model_name, info.model as u8);
    println!("  PCB Number: {}", pcb);
    println!("  Dives:      {}", dive_count);

    conn.disconnect().await?;
    Ok(())
}

// ── Download ──

async fn cmd_download(
    address: Option<String>,
    output: PathBuf,
    format: OutputFormat,
    save_raw: Option<PathBuf>,
) -> Result<()> {
    // Load existing dives from output file (if any) for incremental download
    let mut existing_dives: Vec<DiveLog> = Vec::new();
    let mut existing_numbers: HashSet<u32> = HashSet::new();

    if matches!(format, OutputFormat::Json) && output.exists() {
        match std::fs::read_to_string(&output) {
            Ok(contents) => match serde_json::from_str::<DiveData>(&contents) {
                Ok(data) => {
                    for dive in &data.dives {
                        existing_numbers.insert(dive.number);
                    }
                    eprintln!(
                        "Loaded {} existing dive(s) from {}",
                        data.dives.len(),
                        output.display()
                    );
                    existing_dives = data.dives;
                }
                Err(e) => {
                    eprintln!("Warning: could not parse {}: {e}", output.display());
                }
            },
            Err(e) => {
                eprintln!("Warning: could not read {}: {e}", output.display());
            }
        }
    }

    let adapter = ble::get_adapter().await?;
    let peripheral = find_device(&adapter, address.as_deref()).await?;
    let mut conn = ble::connect(&peripheral, None, None).await?;

    let info = protocol::get_device_info(&mut conn).await?;
    eprintln!("Connected to {}", info.model_name);

    // Set datetime
    if let Err(e) = protocol::set_datetime(&mut conn).await {
        eprintln!("Warning: could not set datetime: {e}");
    }

    // Count dives
    let dive_count = protocol::count_dives(&mut conn).await?;
    eprintln!("Found {} dive(s)", dive_count);

    if dive_count == 0 {
        eprintln!("No dives on device.");
        conn.disconnect().await?;
        return Ok(());
    }

    // Download dive headers + profiles, skipping already-downloaded dives
    let mut new_dives = Vec::new();
    let mut skipped = 0u32;

    for i in 0..dive_count {
        eprint!("\rChecking dive {}/{}...", i + 1, dive_count);

        let header = protocol::read_dive_header(&mut conn, i).await?;

        // Check if we already have this dive
        let dive_number = parser::dive_number_from_header(&header);
        if existing_numbers.contains(&dive_number) {
            eprintln!("\r  Dive #{}: already downloaded, skipping", dive_number);
            skipped += 1;
            continue;
        }

        eprint!("\rDownloading dive {}/{}...", i + 1, dive_count);
        let profile = protocol::read_dive_profile(&mut conn, i).await?;

        if let Some(ref raw_dir) = save_raw {
            std::fs::create_dir_all(raw_dir)?;
            std::fs::write(raw_dir.join(format!("dive_{i:03}_header.bin")), &header)?;
            std::fs::write(raw_dir.join(format!("dive_{i:03}_profile.bin")), &profile)?;
        }

        match parser::parse_dive_ecop(i as u32, &header, &profile) {
            Ok(dive) => {
                eprintln!(
                    "\r  Dive #{}: {} | {:.1}m | {}s | {} samples",
                    dive.number,
                    dive.datetime.format("%Y-%m-%d %H:%M"),
                    dive.max_depth_m,
                    dive.duration_seconds,
                    dive.samples.len(),
                );
                new_dives.push(dive);
            }
            Err(e) => {
                eprintln!("\r  Dive {i}: parse error: {e}");
            }
        }
    }
    eprintln!();

    conn.disconnect().await?;

    if skipped > 0 {
        eprintln!("Skipped {} already-downloaded dive(s)", skipped);
    }
    if !new_dives.is_empty() {
        eprintln!("Downloaded {} new dive(s)", new_dives.len());
    }

    // Merge existing + new dives
    let mut all_dives = existing_dives;
    all_dives.append(&mut new_dives);
    all_dives.sort_by_key(|d| d.number);

    if all_dives.is_empty() {
        eprintln!("No dives could be parsed.");
        return Ok(());
    }

    // Export
    match format {
        OutputFormat::Json => {
            let data = DiveData { dives: all_dives };
            let json = serde_json::to_string_pretty(&data)?;
            std::fs::write(&output, &json)?;
            eprintln!("Dive data saved to {} ({} dives)", output.display(), data.dives.len());
        }
        OutputFormat::Csv => {
            for dive in &all_dives {
                let stem = output
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();
                let dir = output.parent().unwrap_or(std::path::Path::new("."));
                let csv_path = dir.join(format!("{}_{:03}.csv", stem, dive.number));
                let csv = parser::dive_to_csv(dive);
                std::fs::write(&csv_path, &csv)?;
                eprintln!("  Dive #{} -> {}", dive.number, csv_path.display());
            }
        }
    }

    Ok(())
}

// ── Parse (offline) ──

fn cmd_parse(raw_dir: PathBuf, output: PathBuf, format: OutputFormat) -> Result<()> {
    // Count available dives
    let mut dive_count = 0u16;
    while raw_dir.join(format!("dive_{:03}_header.bin", dive_count)).exists() {
        dive_count += 1;
    }

    if dive_count == 0 {
        anyhow::bail!("No dive files found in {}", raw_dir.display());
    }

    eprintln!("Found {} raw dive file(s) in {}", dive_count, raw_dir.display());

    let mut dives = Vec::new();
    for i in 0..dive_count {
        let header = std::fs::read(raw_dir.join(format!("dive_{i:03}_header.bin")))?;
        let profile = std::fs::read(raw_dir.join(format!("dive_{i:03}_profile.bin")))?;

        match parser::parse_dive_ecop(i as u32, &header, &profile) {
            Ok(dive) => {
                eprintln!(
                    "  Dive #{}: {} | {:.1}m | {}min | {} samples | {:?}",
                    dive.number,
                    dive.datetime.format("%Y-%m-%d %H:%M"),
                    dive.max_depth_m,
                    dive.duration_seconds / 60,
                    dive.samples.len(),
                    dive.dive_mode,
                );
                dives.push(dive);
            }
            Err(e) => {
                eprintln!("  Dive {i}: parse error: {e}");
            }
        }
    }

    if dives.is_empty() {
        eprintln!("No dives could be parsed.");
        return Ok(());
    }

    eprintln!("Parsed {} dive(s)", dives.len());

    match format {
        OutputFormat::Json => {
            let data = DiveData { dives };
            let json = serde_json::to_string_pretty(&data)?;
            std::fs::write(&output, &json)?;
            eprintln!("Dive data saved to {}", output.display());
        }
        OutputFormat::Csv => {
            for dive in &dives {
                let stem = output
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();
                let dir = output.parent().unwrap_or(std::path::Path::new("."));
                let csv_path = dir.join(format!("{}_{:03}.csv", stem, dive.number));
                let csv = parser::dive_to_csv(dive);
                std::fs::write(&csv_path, &csv)?;
                eprintln!("  Dive #{} -> {}", dive.number, csv_path.display());
            }
        }
    }

    Ok(())
}

// ── Helpers ──

/// Find a Mares device, either by address or by scanning.
async fn find_device(
    adapter: &btleplug::platform::Adapter,
    address: Option<&str>,
) -> Result<btleplug::platform::Peripheral> {
    eprintln!("Scanning for Mares devices...");
    let devices = ble::scan_for_devices(adapter, Duration::from_secs(10)).await?;

    if devices.is_empty() {
        anyhow::bail!("No Mares devices found. Make sure the dive computer is in Bluetooth mode.");
    }

    let dev = if let Some(addr) = address {
        let addr_upper = addr.to_uppercase();
        devices
            .into_iter()
            .find(|d| d.address.to_uppercase() == addr_upper)
            .with_context(|| format!("Device with address {addr} not found"))?
    } else {
        eprintln!("Connecting to first device: {}", devices[0].name);
        devices.into_iter().next().unwrap()
    };

    Ok(dev.peripheral)
}
