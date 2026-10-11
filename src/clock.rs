//! TAI time as NMOS uses it: `<seconds>:<nanoseconds>` since the TAI epoch.
//!
//! IS-05 scheduled activations and IS-04 `version` values are TAI. On Linux the
//! crate reads `CLOCK_TAI`. Elsewhere (Strom also builds for Windows and macOS) it
//! uses the realtime clock plus the TAI−UTC offset of 37 s.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// TAI−UTC since 1 January 2017.
pub const TAI_UTC_OFFSET_SECS: u64 = 37;

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// A TAI timestamp, or a TAI duration for relative activations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaiTime {
    /// Whole seconds.
    pub secs: u64,
    /// Nanoseconds, below one second.
    pub nanos: u32,
}

impl TaiTime {
    /// `secs` seconds and `nanos` nanoseconds; `nanos` above one second carry over.
    pub fn new(secs: u64, nanos: u32) -> Self {
        Self::from_nanos(u128::from(secs) * NANOS_PER_SEC + u128::from(nanos))
    }

    /// Nanoseconds since the epoch.
    pub fn as_nanos(self) -> u128 {
        u128::from(self.secs) * NANOS_PER_SEC + u128::from(self.nanos)
    }

    /// The timestamp `nanos` nanoseconds after the epoch.
    pub fn from_nanos(nanos: u128) -> Self {
        Self {
            secs: (nanos / NANOS_PER_SEC) as u64,
            nanos: (nanos % NANOS_PER_SEC) as u32,
        }
    }

    /// This time plus `duration`.
    pub fn plus(self, duration: Duration) -> Self {
        Self::from_nanos(self.as_nanos() + duration.as_nanos())
    }

    /// The smallest step after this time.
    pub fn next(self) -> Self {
        Self::from_nanos(self.as_nanos() + 1)
    }

    /// How long from `earlier` until this time; zero when `earlier` is later.
    pub fn since(self, earlier: TaiTime) -> Duration {
        let nanos = self.as_nanos().saturating_sub(earlier.as_nanos());
        Duration::new(
            (nanos / NANOS_PER_SEC) as u64,
            (nanos % NANOS_PER_SEC) as u32,
        )
    }

    /// As a duration, for relative activation offsets.
    pub fn as_duration(self) -> Duration {
        Duration::new(self.secs, self.nanos)
    }
}

impl fmt::Display for TaiTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.secs, self.nanos)
    }
}

/// The text is not `<seconds>:<nanoseconds>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTaiError(String);

impl fmt::Display for ParseTaiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}' is not a TAI timestamp <seconds>:<nanoseconds>", self.0)
    }
}

impl std::error::Error for ParseTaiError {}

impl FromStr for TaiTime {
    type Err = ParseTaiError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let err = || ParseTaiError(text.to_string());
        let (secs, nanos) = text.split_once(':').ok_or_else(err)?;
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits(secs) || !digits(nanos) {
            return Err(err());
        }
        let secs: u64 = secs.parse().map_err(|_| err())?;
        let nanos: u64 = nanos.parse().map_err(|_| err())?;
        if nanos >= NANOS_PER_SEC as u64 {
            return Err(err());
        }
        Ok(Self::new(secs, nanos as u32))
    }
}

/// Source of TAI time. Tests inject their own.
pub trait Clock: Send + Sync + 'static {
    /// The current TAI time.
    fn now(&self) -> TaiTime;
}

/// The system's TAI clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> TaiTime {
        system_tai_now()
    }
}

/// Shared clock handle.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(target_os = "linux")]
fn system_tai_now() -> TaiTime {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid timespec and CLOCK_TAI is a valid clock id on Linux.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_TAI, &mut ts) };
    if rc == 0 && ts.tv_sec >= 0 {
        return TaiTime::new(ts.tv_sec as u64, ts.tv_nsec as u32);
    }
    realtime_plus_offset()
}

#[cfg(not(target_os = "linux"))]
fn system_tai_now() -> TaiTime {
    realtime_plus_offset()
}

fn realtime_plus_offset() -> TaiTime {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    TaiTime::new(
        since_epoch.as_secs() + TAI_UTC_OFFSET_SECS,
        since_epoch.subsec_nanos(),
    )
}

/// The kernel's TAI−UTC offset in seconds (`CLOCK_TAI − CLOCK_REALTIME`, rounded).
///
/// 37 on a correctly configured host. 0 means the kernel's TAI offset is unset and
/// every TAI time is 37 s off. Always 37 on non-Linux builds.
pub fn tai_utc_offset_secs() -> i64 {
    let realtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let tai = system_tai_now();
    let diff = tai.as_nanos() as i128 - realtime.as_nanos() as i128;
    ((diff + diff.signum() * 500_000_000) / 1_000_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        let t: TaiTime = "1439299836:10".parse().unwrap();
        assert_eq!(t, TaiTime::new(1439299836, 10));
        assert_eq!(t.to_string(), "1439299836:10");
        assert!("1:".parse::<TaiTime>().is_err());
        assert!("a:1".parse::<TaiTime>().is_err());
        assert!("1:1000000000".parse::<TaiTime>().is_err());
        assert!("-1:0".parse::<TaiTime>().is_err());
    }

    #[test]
    fn arithmetic() {
        let t = TaiTime::new(10, 999_999_999);
        assert_eq!(t.next(), TaiTime::new(11, 0));
        assert_eq!(t.plus(Duration::from_nanos(2)), TaiTime::new(11, 1));
        assert_eq!(TaiTime::new(12, 0).since(t), Duration::new(1, 1));
        assert_eq!(t.since(TaiTime::new(12, 0)), Duration::ZERO);
    }

    #[test]
    fn system_clock_is_tai_not_utc() {
        let utc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let tai = SystemClock.now().secs;
        // Either the kernel offset is set (37 s) or this is the fallback (also 37 s).
        // A host with an unset kernel offset reads 0 here; accept it but no other value.
        let offset = tai as i64 - utc as i64;
        assert!(
            (36..=38).contains(&offset) || (-1..=1).contains(&offset),
            "{offset}"
        );
    }
}
