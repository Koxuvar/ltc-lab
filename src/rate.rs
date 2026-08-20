//! Frame-rate handling shared by all frontends. A rate carries a NOMINAL count
//! (for numbering) and a REAL rate (for sample timing); drop-frame applies only
//! to the 29.97 family.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateSpec {
    pub nominal: u8,
    pub real: f64,
    pub drop: bool,
}

impl RateSpec {
    /// Short label: "30", "29.97", etc.
    pub fn label(&self) -> String {
        if (self.real - self.real.round()).abs() < 1e-6 {
            format!("{}", self.nominal)
        } else {
            format!("{:.2}", self.real)
        }
    }
}

pub const PRESETS: [&str; 5] = ["24", "25", "30", "29.97", "23.976"];

pub fn parse_rate(fps: &str, start_is_df: bool) -> Result<RateSpec, String> {
    let df_2997 = 30_000.0 / 1001.0;
    let df_2398 = 24_000.0 / 1001.0;
    let spec = match fps {
        "24" => RateSpec {
            nominal: 24,
            real: 24.0,
            drop: false,
        },
        "25" => RateSpec {
            nominal: 25,
            real: 25.0,
            drop: false,
        },
        "30" if start_is_df => RateSpec {
            nominal: 30,
            real: df_2997,
            drop: true,
        },
        "30" => RateSpec {
            nominal: 30,
            real: 30.0,
            drop: false,
        },
        "29.97" | "29.97df" => RateSpec {
            nominal: 30,
            real: df_2997,
            drop: start_is_df,
        },
        "23.976" | "23.98" => RateSpec {
            nominal: 24,
            real: df_2398,
            drop: false,
        },
        other => {
            return Err(format!(
                "unsupported fps {other:?} (try 24, 25, 30, 29.97, 23.976)"
            ))
        }
    };
    if start_is_df && !spec.drop {
        return Err(format!(
            "drop-frame (';') is only defined for 29.97, not {fps} fps"
        ));
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse() {
        for p in PRESETS {
            assert!(parse_rate(p, false).is_ok(), "{p} should parse");
        }
    }

    #[test]
    fn df_inferred_from_semicolon_on_30() {
        let r = parse_rate("30", true).unwrap();
        assert!(r.drop && r.nominal == 30);
        assert!((r.real - 30_000.0 / 1001.0).abs() < 1e-6);
    }

    #[test]
    fn df_rejected_on_nondrop_rate() {
        assert!(parse_rate("25", true).is_err());
        assert!(parse_rate("24", true).is_err());
    }

    #[test]
    fn nondrop_2997_allowed() {
        let r = parse_rate("29.97", false).unwrap();
        assert!(!r.drop && r.nominal == 30);
    }

    #[test]
    fn label_shows_fractional() {
        assert_eq!(parse_rate("30", false).unwrap().label(), "30");
        assert_eq!(parse_rate("29.97", true).unwrap().label(), "29.97");
    }
}
