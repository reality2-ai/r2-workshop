# Datasheets — 2026-05 hardware shipment

Datasheets + schematics for the DFRobot parts received 2026-05-29, for
evaluation against the rocker structural-health-monitoring use case.
PDFs were pulled from the DFRobot wikis (and bundled chip-datasheet
zips); sources listed at the bottom.

## Core + storage

| Part | What it is | Key specs |
|---|---|---|
| **DFR1117** | Beetle **ESP32-C6** mini dev board | 160 MHz single-core **RISC-V**; Wi-Fi 6 (2.4 GHz), BLE 5, 802.15.4 (Zigbee 3.0 / Thread 1.3 / Matter); ~13 IO; 3.3 V logic, USB-C 5 V in, on-board LiPo charge (0.5 A) + battery-voltage monitoring; 14 µA deep sleep; 25 × 20.5 mm. |
| **DFR0229** | Fermion MicroSD card module | SPI interface; MicroSD/TF; 5 V supply; 20 × 28 mm. Pin-compatible with DFRobot IO expansion shields. |

> **Note — new carrier.** DFR1117 is an **ESP32-C6 (RISC-V)**, distinct
> from the current ESP32-S3 carriers (DevKitC, XIAO). It is a candidate
> *new carrier* in the (class, carrier) sense, and a different
> `compile_target` for the Sensor role-ensemble.

## Candidate accelerometers (pick one as the sensing element)

All four are triaxial MEMS accelerometers on breakout boards. For SHM
the decision axes are **resolution, noise, output data rate (ODR), and
full-scale** — higher resolution / lower noise = better at resolving
small joint-shear vibration.

| SKU | Chip | Full-scale | Resolution | Max ODR | Interface | Notes for SHM |
|---|---|---|---|---|---|---|
| **SEN0405** | ST **LIS2DW12** | ±2/4/8/16 g | up to 14-bit | ~1.6 kHz | I²C (0x19) / SPI | Ultra-low power, low noise (~1.3 mg RMS), 32-deep FIFO, tap/wake/6D engine. Strong mid-tier candidate. |
| **SEN0224** | ST **LIS2DH** (Gravity) | ±2/4/8/16 g | up to 10-bit (hi-res) | ~5.3 kHz | I²C (Gravity 4-pin), 3.3–5 V | Plug-and-play Gravity connector; highest ODR of the four; modest resolution. |
| **SEN0140** | **10-DOF IMU**: ADXL345 (accel) + ITG-3200 (gyro) + BMP085 (baro) + VCM5883L (mag) | accel ±2/4/8/16 g | accel 13-bit | accel ~3.2 kHz | I²C / SPI | Full IMU — gyro+mag are extra signals you don't strictly need, but available. ADXL345 is a classic, well-supported part. |
| **SEN0168** | Bosch **BMA220** | ±2/4/8/16 g | **6-bit** | config | I²C / SPI, 3.3 V | Tiny (2×2 mm chip). **6-bit data is coarse** — likely too low-resolution for fine shear monitoring; better for motion/tilt detection. |

**Reference — the current sensor:** ADXL355 is a precision SHM-grade
part (±2/4/8 g, **20-bit**, ~25 µg/√Hz noise). None of the four above
match its fidelity; they are cheaper / lower-power alternatives. Rough
fidelity order for SHM: LIS2DW12 ≈ ADXL345 > LIS2DH ≫ BMA220, all below
the ADXL355. See each datasheet for the authoritative noise/resolution
figures before choosing.

## File index

```
DFR1117-beetle-esp32-c6-overview-farnell.pdf   board product overview
DFR1117-beetle-esp32-c6-schematics.pdf         board schematic (V1.1)
DFR1117-esp32-c6-chip-datasheet.pdf            Espressif ESP32-C6 chip
DFR1117-rt9080-ldo-datasheet.pdf               on-board LDO regulator
DFR1117-tps62a02-buck-datasheet.pdf            on-board buck regulator

DFR0229-microsd-module-datasheet.pdf           module datasheet
DFR0229-microsd-module-schematics.pdf          module schematic

SEN0405-lis2dw12-datasheet.pdf                 LIS2DW12 chip datasheet
SEN0405-lis2dw12-schematics.pdf                breakout schematic

SEN0224-lis2dh-datasheet.pdf                   LIS2DH chip datasheet
SEN0224-lis2dh-schematics.pdf                  breakout schematic
SEN0224-lis2dh-dimension.pdf                   board dimensions

SEN0140-10dof-imu-schematics.pdf               breakout schematic
SEN0140-adxl345-accel-datasheet.pdf            ADXL345 accelerometer
SEN0140-itg3200-gyro-datasheet.pdf             ITG-3200 gyroscope
SEN0140-bmp085-baro-datasheet.pdf              BMP085 barometer
SEN0140-vcm5883l-mag-datasheet.pdf             VCM5883L magnetometer

SEN0168-bma220-datasheet.pdf                   BMA220 chip datasheet
SEN0168-bma220-schematics.pdf                  breakout schematic
```

## Sources

- DFR1117 Beetle ESP32-C6 — https://wiki.dfrobot.com/SKU_DFR1117_Beetle_ESP32_C6
- DFR0229 MicroSD module — https://wiki.dfrobot.com/dfr0229/
- SEN0140 10-DOF IMU — https://wiki.dfrobot.com/10_DOF_Mems_IMU_Sensor_V2.0_SKU__SEN0140
- SEN0405 LIS2DW12 — https://wiki.dfrobot.com/sen0405/
- SEN0168 BMA220 — https://wiki.dfrobot.com/Triple_Axis_Accelerometer_BMA220_Tiny__SKU_SEN0168
- SEN0224 LIS2DH (Gravity) — https://wiki.dfrobot.com/Gravity__I2C_Triple_Axis_Accelerometer_-_LIS2DH_SKU_SEN0224
