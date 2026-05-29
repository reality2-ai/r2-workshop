//! ST LIS2DH I²C driver (DFRobot SEN0224, Gravity).
//!
//! The DFR1117 / SEN0224 sensing element. Drop-in peer to the S3
//! carriers' `adxl355.rs`: same public shape (`new(...)` +
//! `read_xyz_lsb() -> (i32,i32,i32)`), so the sender + sim-fallback path
//! are unchanged. The difference is the bus (I²C, not SPI) and the chip
//! — which is exactly why the sensor is a swappable plugin: it provides
//! the same `accel.triaxial` capability behind a different driver.
//!
//! Wiring per `HARDWARE-WIRING-DFR1117.md` §3 (Gravity I²C) — pads match
//! the board silk: VCC→`3V3`  GND→`GND`  SCL→`SCL`(GPIO20)  SDA→`SDA`(GPIO19)
//! (INT unused; firmware polls).
//!
//! **Units.** `read_xyz_lsb()` returns values in the *same* convention
//! the wire + dashboard assume for the ADXL355: **1 g = 256_000 LSB**
//! (`sim.rs` / WIRE §4.1). The LIS2DH's native counts are rescaled to
//! that convention here, so its coarser resolution shows up honestly as
//! quantisation (steps of 256 LSB at ±2 g HR) rather than a wrong scale.
//!
//! Configured for **high-resolution mode, ±2 g, 400 Hz ODR**. In HR mode
//! at ±2 g the sensitivity is 1 mg / digit, where `digit` is the 12-bit
//! left-justified sample (`raw_i16 >> 4`). So
//!   lsb_256k = digit_mg × 256 = (raw_i16 >> 4) × 256.

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::i2c::I2cDriver;
use log::{info, warn};

/// LIS2DH WHO_AM_I register + expected value (datasheet §8.2).
const REG_WHO_AM_I: u8 = 0x0F;
const EXPECTED_WHO_AM_I: u8 = 0x33;

// Control registers (datasheet §8).
const REG_CTRL1: u8 = 0x20; // ODR + LPen + axis enable
const REG_CTRL4: u8 = 0x23; // BDU + full-scale + HR
// First output register; auto-increment burst-reads X/Y/Z (6 bytes)
// when the sub-address MSB is set.
const REG_OUT_X_L: u8 = 0x28;
const AUTO_INC: u8 = 0x80;

// CTRL_REG1 = 0b0111_0111: ODR=400 Hz (0111), LPen=0 (HR/normal), Z/Y/X enabled.
const CTRL1_400HZ_XYZ: u8 = 0x77;
// CTRL_REG4 = 0b1000_1000: BDU=1, FS=±2 g (00), HR=1 (high-resolution, 12-bit).
const CTRL4_BDU_HR_2G: u8 = 0x88;

/// Candidate 7-bit I²C addresses — LIS2DH SA0 strap selects 0x18 or 0x19.
const ADDR_CANDIDATES: [u8; 2] = [0x18, 0x19];

/// At ±2 g HR the 12-bit sample is 1 mg/digit; ×256 maps mg → the
/// 256_000-LSB/g convention shared with the ADXL355.
const LSB_PER_DIGIT_2G_HR: i32 = 256;

/// LIS2DH on an owned I²C bus. Built inside the sender/diag thread (to
/// match the SPI driver's thread-local lifetime), lives for the
/// program's duration.
pub struct Lis2dh {
    i2c: I2cDriver<'static>,
    addr: u8,
}

impl Lis2dh {
    /// Take ownership of a pre-initialised I²C bus, find the chip
    /// (WHO_AM_I at 0x18 or 0x19), and configure HR / ±2 g / 400 Hz.
    pub fn new(i2c: I2cDriver<'static>) -> Result<Self> {
        let mut dev = Self { i2c, addr: ADDR_CANDIDATES[0] };

        // Probe both SA0 addresses for the expected WHO_AM_I.
        let mut found = None;
        for &a in &ADDR_CANDIDATES {
            dev.addr = a;
            match dev.read_reg(REG_WHO_AM_I) {
                Ok(id) => {
                    info!("[LIS2DH] WHO_AM_I @0x{a:02X} = 0x{id:02X}");
                    if id == EXPECTED_WHO_AM_I {
                        found = Some(a);
                        break;
                    }
                }
                Err(e) => warn!("[LIS2DH] no ACK @0x{a:02X}: {e}"),
            }
        }
        let addr = found.ok_or_else(|| {
            anyhow!("WHO_AM_I != 0x33 at either 0x18/0x19 — LIS2DH not found on I²C")
        })?;
        dev.addr = addr;
        info!("[LIS2DH] found at 0x{addr:02X} ✓");

        // High-resolution, ±2 g, 400 Hz, all axes, block-data-update.
        dev.write_reg(REG_CTRL1, CTRL1_400HZ_XYZ)?;
        dev.write_reg(REG_CTRL4, CTRL4_BDU_HR_2G)?;
        info!("[LIS2DH] configured: HR mode, ±2 g, 400 Hz ODR");

        Ok(dev)
    }

    fn read_reg(&mut self, reg: u8) -> Result<u8> {
        let mut buf = [0u8; 1];
        self.i2c
            .write_read(self.addr, &[reg], &mut buf, BLOCK)
            .with_context(|| format!("LIS2DH I²C read_reg 0x{reg:02X}"))?;
        Ok(buf[0])
    }

    fn write_reg(&mut self, reg: u8, val: u8) -> Result<()> {
        self.i2c
            .write(self.addr, &[reg, val], BLOCK)
            .with_context(|| format!("LIS2DH I²C write_reg 0x{reg:02X}"))?;
        Ok(())
    }

    /// Read X / Y / Z in one auto-incrementing burst, returned in the
    /// 256_000-LSB/g convention (see module docs).
    pub fn read_xyz_lsb(&mut self) -> Result<(i32, i32, i32)> {
        let mut buf = [0u8; 6];
        self.i2c
            .write_read(self.addr, &[REG_OUT_X_L | AUTO_INC], &mut buf, BLOCK)
            .context("LIS2DH I²C burst read OUT_X..OUT_Z")?;
        let x = decode_axis(buf[0], buf[1]);
        let y = decode_axis(buf[2], buf[3]);
        let z = decode_axis(buf[4], buf[5]);
        Ok((x, y, z))
    }
}

/// Decode one axis from its low+high output bytes. The LIS2DH OUT
/// registers are 16-bit left-justified; in HR mode the significant
/// 12 bits are `raw_i16 >> 4` (arithmetic, sign-preserving). Rescale to
/// the 256_000-LSB/g convention.
fn decode_axis(lo: u8, hi: u8) -> i32 {
    let raw = i16::from_le_bytes([lo, hi]);
    let digit12 = (raw >> 4) as i32; // 12-bit signed, 1 mg/digit at ±2 g HR
    digit12 * LSB_PER_DIGIT_2G_HR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_zero() {
        assert_eq!(decode_axis(0x00, 0x00), 0);
    }

    #[test]
    fn decode_one_g() {
        // 1 g at ±2 g HR = 1000 digits → left-justified 16-bit = 1000<<4.
        let raw = (1000i16) << 4;
        let [lo, hi] = raw.to_le_bytes();
        assert_eq!(decode_axis(lo, hi), 256_000); // == ADXL355 1 g
    }

    #[test]
    fn decode_negative_one_g() {
        let raw = (-1000i16) << 4;
        let [lo, hi] = raw.to_le_bytes();
        assert_eq!(decode_axis(lo, hi), -256_000);
    }
}
