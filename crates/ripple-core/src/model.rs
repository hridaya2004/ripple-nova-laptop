//! Model detection from the EC project id, with DMI as a fallback hint.
//! Never guesses a "nearest" model.

use crate::fan::model_dir_for;

/// OEM `Define.ProjectID` values reachable from the single byte at EC 1856.
///
/// Left out on purpose, these resolve via DMI only:
/// * 1..=7: the 16-bit PH4A*/PH6A* ids are 0x1701..=0x1707, whose low byte is
///   1..=7, so a one-byte read cannot tell them from the legacy GI/GJ/GK ids.
/// * 23 (PH4AQxx): hypothesis, unverified: 23 is also 0x17, the high byte of
///   those 16-bit ids, so reg 1856 = 23 might be the family with the sub-id
///   elsewhere (candidate: `ADDR_OEMSERVICE_PROJECT_ID_BYTE`, reg 1868).
const PROJECTS: &[(u8, &str)] = &[
    (8, "GK7CPCS_GK5CQ7Z"),
    (9, "PF"),
    (10, "GK5CP_4X_5X_6X"),
    (11, "IDP"),
    (12, "IDY_6Y"),
    (13, "IDY_7Y"),
    (14, "PF4MU_PF4MN_PF5MU"),
    (15, "CML_Gaming"),
    (16, "GK7NXXR"),
    (17, "GM5MU1Y"),
    (18, "PH4TRX1"),
    (19, "PH4TUX1"),
    (20, "PH4TQx1"),
    (21, "PH6TRX1"),
    (22, "PH6TQxx"),
    (24, "PH4PQxx"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Ec,
    Dmi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// OEM project name, `None` if neither the EC id nor DMI identified it.
    pub name: Option<&'static str>,
    pub source: Option<Source>,
    /// Raw byte read from EC 1856 (if the EC was readable).
    pub ec_id: Option<u8>,
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.name, self.source, self.ec_id) {
            (Some(n), Some(Source::Ec), Some(id)) => write!(f, "{n} (EC project id {id})"),
            (Some(n), _, _) => write!(f, "{n} (from DMI)"),
            (None, _, Some(id)) => write!(f, "unknown (EC project id {id})"),
            (None, _, None) => write!(f, "unknown (EC not readable)"),
        }
    }
}

/// EC project id wins; DMI strings are only consulted when it doesn't resolve.
pub fn detect(ec_id: Option<u8>, dmi: &[String]) -> Model {
    if let Some(name) = ec_id.and_then(|id| PROJECTS.iter().find(|(i, _)| *i == id)) {
        return Model {
            name: Some(name.1),
            source: Some(Source::Ec),
            ec_id,
        };
    }
    let name = dmi.iter().find_map(|f| model_dir_for(f));
    Model {
        name,
        source: name.map(|_| Source::Dmi),
        ec_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Vec<String> {
        vec![v.to_string()]
    }

    #[test]
    fn ec_id_16_is_gk7nxxr_even_when_dmi_is_generic() {
        let m = detect(Some(16), &s("Standard"));
        assert_eq!(m.name, Some("GK7NXXR"));
        assert_eq!(m.source, Some(Source::Ec));
        assert_eq!(m.to_string(), "GK7NXXR (EC project id 16)");
    }

    #[test]
    fn ec_wins_over_dmi() {
        assert_eq!(detect(Some(18), &s("PH6ARxx")).name, Some("PH4TRX1"));
    }

    #[test]
    fn ambiguous_low_ids_fall_back_to_dmi() {
        let m = detect(Some(1), &s("XMG PH4ARXX"));
        assert_eq!((m.name, m.source), (Some("PH4ARxx"), Some(Source::Dmi)));
    }

    #[test]
    fn id_23_is_not_trusted_from_the_ec_alone() {
        assert_eq!(detect(Some(23), &s("Standard")).name, None);
        assert_eq!(detect(Some(23), &s("PH4AQxx")).source, Some(Source::Dmi));
    }

    #[test]
    fn unknown_stays_unknown() {
        let m = detect(Some(99), &s("Standard"));
        assert_eq!(m.name, None);
        assert_eq!(m.to_string(), "unknown (EC project id 99)");
        assert_eq!(detect(None, &[]).to_string(), "unknown (EC not readable)");
    }
}
