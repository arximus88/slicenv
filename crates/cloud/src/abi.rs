//! Покоління ABI закритого плагіна Bambu, які вміє викликати шим.
//!
//! Дзеркало `abi_of` у `native/shim.cpp` (там рішення остаточне, бо там макети структур).
//! Джерело — таблиця `AVAILABLE_NETWORK_VERSIONS` в Orca (`bambu_networking.hpp`): ABI стабільний
//! у межах серії AA.BB.CC, тож 02.08.04.xx — одне покоління. 02.06 Orca не має; його макет
//! звірено з Bambu Studio v02.06.00.51, тому дозволена лише точна збірка 02.06.00.50.
//! Legacy (01.10.01) і 02.03 в Orca є, але шим їх не реалізує й наживо їх не перевіряли —
//! такі плагіни відхиляються з чіткою помилкою.

/// Покоління, через яке шим викликає плагін.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    /// 02.06.00.50: `PrintParams` без `svc_context`/`slicer_uid`/`queue_plate_id`.
    V0206,
    /// 02.08.04.xx — поточна серія Bambu Studio/Orca; `PrintParams` з трьома новими рядками.
    V0208,
}

/// Серія, яку завантажує менеджер плагіна (як `get_latest_network_version` в Orca).
pub const LATEST_SERIES: &str = "02.08.04";

/// Для повідомлень користувачеві.
pub const SUPPORTED: &str = "02.08.04.xx, 02.06.00.50";

pub fn generation(version: &str) -> Option<Generation> {
    if version == "02.06.00.50" {
        return Some(Generation::V0206);
    }
    let build = version.strip_prefix("02.08.04.")?;
    (build.len() == 2 && build.bytes().all(|b| b.is_ascii_digit())).then_some(Generation::V0208)
}

/// Чи має рядок форму AA.BB.CC.DD (лише цифри) — інакше його не можна вживати в шляхах.
pub fn is_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn unsupported_message(version: &str) -> String {
    let shown = if is_version(version) {
        version
    } else {
        "невідома"
    };
    format!("Версія плагіна Bambu {shown} не підтримується; підтримуються {SUPPORTED}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_matches_the_shim_gate() {
        assert_eq!(generation("02.06.00.50"), Some(Generation::V0206));
        assert_eq!(generation("02.06.00.51"), None);
        assert_eq!(generation("02.08.04.60"), Some(Generation::V0208));
        assert_eq!(generation("02.08.04.57"), Some(Generation::V0208));
        assert_eq!(generation("02.08.04.6"), None);
        assert_eq!(generation("02.08.01.53"), None);
        assert_eq!(generation("02.03.00.62"), None);
        assert_eq!(generation("01.10.01.01"), None);
        assert!(is_version("02.08.04.60") && !is_version("02.08.04.60/../x"));
        let msg = unsupported_message("01.10.01.01");
        assert!(msg.contains("01.10.01.01") && msg.contains("02.08.04"));
        assert!(!unsupported_message("../../etc").contains("etc"));
    }
}
