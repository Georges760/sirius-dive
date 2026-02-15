# Mares Sirius BLE Protocol - Research Findings

## Device Info
- **Model**: Sirius (GENIUS family, model ID 0x2F in libdivecomputer)
- **Firmware**: 01.08.01
- **PCB Number**: 9771002219******
- **BLE Address**: B4:E3:F9:**:**:**

## GATT Profile

### Service 1: `544e326b-5b72-c6b0-1c46-41c1bc448118` (Mares ECOP protocol)
| Characteristic | Properties | Role |
|---|---|---|
| `99a91ebd-b21f-1689-bb43-681f1f55e966` | READ, WRITE_WITHOUT_RESPONSE | Write commands to device |
| `1d1aae28-d2a8-91a1-1242-9d2973fbe571` | READ, NOTIFY | Receive responses (notifications) |
| `d8b3ab7c-4101-ec80-c441-9b0914f6ebc3` | READ | Static value `[01 00]` (protocol version?) |

### Service 2: `1d14d6ee-fd63-4fa1-bfa4-8f47b42119f0` (unknown purpose)
| Characteristic | Properties | Role |
|---|---|---|
| `984227f3-34fc-4045-a5d0-2c581f81a153` | WRITE_WITHOUT_RESPONSE, WRITE | Unknown |
| `f7bf3564-fb6d-4e53-88a4-5e37e0326063` | WRITE | Unknown |

## Protocol: ECOP (CANopen SDO over BLE)

The Sirius uses **ECOP** (Electronic Computer Object Protocol), a CANopen-like SDO (Service Data Object) transfer protocol over BLE. This was discovered by analyzing the SSI Flutter app's logcat output.

### Command Frame Format

Commands are sent as `[cmd, cmd ^ 0xA5]` (2-byte header). Responses arrive as BLE notifications framed with `ACK (0xAA)` ... `END (0xEA)`.

### Commands

| Cmd | Name | Payload | Description |
|---|---|---|---|
| `0xC2` | CMD_VERSION | None | Query device info (model, firmware, serial) - 140 bytes |
| `0xBF` | CMD_SDO_UPLOAD | 18 bytes | Initiate SDO read of an object |
| `0xAC` | CMD_SDO_SEGMENT_0 | None | Read segment with toggle=0 |
| `0xFE` | CMD_SDO_SEGMENT_1 | None | Read segment with toggle=1 |
| `0xB0` | CMD_SET_DATETIME | 4 bytes | Set device clock (LE Unix timestamp) |

### BF (SDO Upload) Protocol

**Payload format** (18 bytes):
```
[0x40, index_lo, index_hi, sub_index, 0x00 * 14]
```

**Response format** (17 bytes between AA/EA):
```
[status, index_lo, index_hi, sub_index, data[12]]
```

**Status codes:**
- `0x41` (SEGMENTED): Data too large for single response. Bytes 4-5 = LE u16 total data size. Use AC/FE segments to read.
- `0x42` (EXPEDITED): Data fits in response. Bytes 4-15 = 12 data bytes directly.
- `0x80` (ABORT): Object not found.

### Segmented Transfer (AC/FE)

For large objects (status 0x41), data is read via alternating segment commands:
1. Send `AC` (toggle=0) -> receive up to 241 bytes
2. Send `FE` (toggle=1) -> receive up to 241 bytes
3. Alternate until all data received

Each segment response: `[AA, toggle_byte, data..., EA]`

### Object Dictionary

| Index | Sub | Size | Type | Description |
|---|---|---|---|---|
| 0x2000 | 4 | 16 bytes | Segmented | PCB number ("9771002219000605") |
| 0x2000 | 8 | 12 bytes | Expedited | Warranty info |
| 0x2006 | 12 | varies | Expedited | Dive mode name ("FREEDIVE") |
| 0x2008 | 1 | 12 bytes | Expedited | Unknown ("1") |
| 0x3000+i | 4 | 200 bytes | Segmented | Dive header for dive index i |
| 0x3000+i | 3 | variable | Segmented | Dive profile for dive index i |

Dive objects are numbered starting at 0x3000. To count dives, attempt BF reads on 0x3000, 0x3001, etc. until 0x80 (abort) is returned.

## Dive Header Format (200 bytes, GENIUS layout)

Source: libdivecomputer `mares_iconhd_parser.c`

| Offset | Size | Field | Units / Notes |
|---|---|---|---|
| 0x00 | 2 | type | Must be 1 (LE u16) |
| 0x02 | 1 | version minor | e.g. 0x00 |
| 0x03 | 1 | version major | e.g. 0x02 |
| 0x04 | 4 | dive_number | LE u32 (sequential, e.g. 49) |
| 0x08 | 4 | datetime | Packed bitfield (see below) |
| 0x0C | 4 | settings | LE u32 (mode, salinity, surftime) |
| 0x20 | 2 | nsamples | LE u16 (number of DPRS records) |
| 0x22 | 2 | max_depth | LE u16, 1/10 meter |
| 0x26 | 2 | temperature_max | LE u16, 1/10 deg C |
| 0x28 | 2 | temperature_min | LE u16, 1/10 deg C |
| 0x3E | 2 | atmospheric | LE u16, 1/1000 bar |
| 0x54 | 100 | gas mixes / tanks | 5 entries, 20 bytes each |

### Packed Datetime (offset 0x08)

32-bit LE integer with bitfield layout:
```
bits  0-4:  hour (0-23)
bits  5-10: minute (0-59)
bits 11-15: day (1-31)
bits 16-19: month (1-12)
bits 20-31: year (absolute, e.g. 2025)
```

Example: `EC D0 9A 7E` -> LE u32 = 0x7E9AD0EC -> 2025-10-26 12:07

### Settings (offset 0x0C)

```
bits  0-3:  dive mode (0=Air, 1=EANx, 2=EANx Multi, 3=Trimix, 4=Gauge, 5=Freedive, 6=SCR, 7=OC)
bits  5-6:  salinity (0=Fresh, 1=Salt, 2=EN13319)
bits 13-18: surface timeout in minutes
```

### Gas Mix Entry (20 bytes each, 5 entries at offset 0x54)

```
offset +0:  gasmixparams (u32 LE)
  bits  0-6:  O2 %
  bits  7-13: N2 %
  bits 14-20: He %
  bits 21-22: state (0=OFF, 1=READY, 2=INUSE, 3=IGNORED)
offset +4:  begin pressure (u16 LE, 1/100 bar)
offset +6:  end pressure (u16 LE, 1/100 bar)
offset +8:  volume (u16 LE)
offset +10: working pressure (u16 LE)
```

### Duration Calculation

Duration is NOT stored directly. Computed as:
```
duration = nsamples * 5 - surftime_minutes * 60
```
(GENIUS family uses fixed 5-second sample interval)

## Dive Profile Format (Tagged Records)

Profile data starts with a 4-byte version header, then a stream of tagged records.

### Record Types

| Tag | Size | Description |
|---|---|---|
| DSTR | 58 bytes | Dive Start Record |
| TISS | 138 bytes | Tissue loading snapshot |
| DPRS | 34 bytes | Depth/Pressure/Temperature sample |
| AIRS | 16 bytes | Air integration (tank pressure) |
| DEND | 162 bytes | Dive End Record |

Each record has:
- 4-byte ASCII tag at start
- Payload
- 2-byte CRC16-CCITT
- 4-byte ASCII tag repeated at end

### DPRS Record Layout (34 bytes)

```
bytes  0-3:  tag "DPRS"
bytes  4-5:  depth (u16 LE, 1/10 meter)
bytes  6-7:  unknown
bytes  8-9:  temperature (u16 LE, 1/10 deg C)
bytes 10-11: unknown
bytes 12-13: unknown
bytes 14-15: deco/NDL time (u16 LE, minutes)
bytes 16-19: alarms (u32 LE, bitmask)
bytes 20-23: unknown
bytes 24-27: misc (u32 LE, packed: gasmix index, bookmark, deco info)
bytes 28-29: CRC16-CCITT
bytes 30-33: tag "DPRS" repeated
```

### AIRS Record Layout (16 bytes)

```
bytes  0-3:  tag "AIRS"
bytes  4-5:  pressure (u16 LE, 1/100 bar)
bytes  6-9:  unknown
bytes 10-11: CRC16-CCITT
bytes 12-15: tag "AIRS" repeated
```

### Typical Profile Sequence

```
[4-byte classifier]
DSTR (58)          -- dive start
TISS (138)         -- initial tissue state
DPRS (34)          -- sample #1 (t=0)
DPRS (34)          -- sample #2 (t=5s)
DPRS (34)          -- sample #3 (t=10s)
DPRS (34)          -- sample #4 (t=15s)
AIRS (16)          -- tank pressure update (every ~4 samples)
DPRS (34)          -- sample #5 (t=20s)
...
DPRS (34)          -- last sample
DSTR (58)          -- dive end start marker
TISS (138)         -- final tissue state
DEND (162)         -- dive end
```

## Version Response Analysis

```
Offset  Data                              Meaning
------  ----                              -------
0x00    00                                Model byte
0x46    53 69 72 69 75 73                 "Sirius" (model name)
0x56    30 31 2E 30 38 2E 30 31          "01.08.01" (firmware string)
0x5E    01 08 01                          Firmware as binary (1.8.1)
0x62    30 36 2D 30 32 2D 32 36          "06-02-26" (manufacturing date?)
0x6C    ** ** ** ** ** ** ** **          "********" (serial string)
```

## Tools & References

- **libdivecomputer**: `mares_iconhd_parser.c` - canonical implementation for GENIUS family parsing
- **btleplug**: Rust BLE library used for communication
