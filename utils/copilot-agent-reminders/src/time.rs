//! Timestamps and elapsed-time values shared by the machine and its adapters.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A Unix-epoch timestamp in milliseconds.
///
/// The epoch itself (`0`) is a valid timestamp. The value is transparent on the
/// wire and in persisted state: it serializes as its numeric millisecond count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct TimestampMillis(u64);

impl TimestampMillis {
    /// Returns the seconds elapsed from `earlier` to `self`.
    ///
    /// A clock that moves backwards has no elapsed time, so the subtraction
    /// saturates at zero instead of underflowing. The whole saturated
    /// millisecond delta is converted to `f64` before division by 1000, which
    /// defines the threshold rounding.
    pub(crate) fn elapsed_seconds_since(self, earlier: Self) -> Seconds {
        let delta_milliseconds = self.0.saturating_sub(earlier.0);
        delta_milliseconds as f64 / 1000.0
    }
}

impl From<u64> for TimestampMillis {
    fn from(milliseconds: u64) -> Self {
        Self(milliseconds)
    }
}

impl fmt::Display for TimestampMillis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A duration expressed in seconds.
///
/// Elapsed-time thresholds and timestamp conversions use this unit. The value
/// is an `f64` numeric seconds representation.
pub(crate) type Seconds = f64;

#[cfg(test)]
mod tests {
    use super::{Seconds, TimestampMillis};
    use serde_json::json;

    #[test]
    fn zero_is_a_valid_timestamp_and_round_trips_as_a_number() {
        let zero = TimestampMillis::from(0);
        let encoded = serde_json::to_value(zero).expect("timestamp should serialize");

        assert_eq!(encoded, json!(0));
        assert_eq!(zero.to_string(), "0");
        assert_eq!(zero.elapsed_seconds_since(zero), 0.0);

        let decoded: TimestampMillis =
            serde_json::from_value(encoded).expect("numeric timestamp should deserialize");
        assert_eq!(decoded, zero);
    }

    #[test]
    fn clock_moving_backwards_elapses_zero_seconds() {
        let earlier = TimestampMillis::from(1_000);
        let later = TimestampMillis::from(2_000);

        assert_eq!(later.elapsed_seconds_since(earlier), 1.0);
        assert_eq!(earlier.elapsed_seconds_since(later), 0.0);
        assert_eq!(
            earlier.elapsed_seconds_since(TimestampMillis::from(u64::MAX)),
            0.0
        );
    }

    #[test]
    fn fractional_seconds_preserve_millisecond_precision() {
        let start = TimestampMillis::from(1_000);

        assert_eq!(
            TimestampMillis::from(1_001).elapsed_seconds_since(start),
            0.001
        );
        assert_eq!(
            TimestampMillis::from(1_499).elapsed_seconds_since(start),
            0.499
        );
        assert_eq!(
            TimestampMillis::from(1_500).elapsed_seconds_since(start),
            0.5
        );

        let elapsed: Seconds = TimestampMillis::from(1_500).elapsed_seconds_since(start);
        assert_eq!(elapsed, 0.5);
    }

    #[test]
    fn maximum_timestamp_has_stable_threshold_rounding() {
        let elapsed =
            TimestampMillis::from(u64::MAX).elapsed_seconds_since(TimestampMillis::from(0));

        assert_eq!(elapsed, 18_446_744_073_709_551_616.0 / 1000.0);
        assert_eq!(elapsed.to_bits(), 0x4350_624d_d2f1_a9fc);
    }
}
