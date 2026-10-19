//! PostgreSQL server versions reviewed for the application's trust boundary.

use super::{Result, StorageError};

const POSTGRES_16_MIN: i32 = 160_015;
const POSTGRES_17_MIN: i32 = 170_011;
const POSTGRES_18_MIN: i32 = 180_006;

pub(super) fn require_supported(version_num: i32) -> Result<()> {
    let supported = match version_num / 10_000 {
        16 => version_num >= POSTGRES_16_MIN,
        17 => version_num >= POSTGRES_17_MIN,
        18 => version_num >= POSTGRES_18_MIN,
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err(StorageError::UnsupportedServerVersion {
            found: display(version_num),
        })
    }
}

fn display(version_num: i32) -> String {
    format!("{}.{}", version_num / 10_000, version_num % 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_reviewed_patched_server_versions() {
        for version in [160_015, 160_099, 170_011, 170_099, 180_006] {
            assert!(require_supported(version).is_ok(), "version {version}");
        }
    }

    #[test]
    fn rejects_vulnerable_or_unreviewed_server_versions() {
        for version in [150_019, 160_014, 170_010, 180_005, 190_000] {
            assert!(require_supported(version).is_err(), "version {version}");
        }
    }
}
