pub(crate) const fn has_exact_configured_service_type(actual: u32, expected: u32) -> bool {
    actual == expected
}

pub(crate) const fn has_allowed_reported_service_type(
    actual: u32,
    base_own_process: u32,
    packaged_own_process: u32,
) -> bool {
    actual == base_own_process || actual == packaged_own_process
}

#[cfg(test)]
mod tests {
    use super::{has_allowed_reported_service_type, has_exact_configured_service_type};

    const WIN32_OWN_PROCESS: u32 = 0x10;
    const WIN32_SHARE_PROCESS: u32 = 0x20;
    const USER_SERVICE: u32 = 0x40;
    const INTERACTIVE_PROCESS: u32 = 0x100;
    const PACKAGED_SERVICE: u32 = 0x200;
    const EXPECTED_PACKAGED_OWN_PROCESS: u32 = WIN32_OWN_PROCESS | PACKAGED_SERVICE;

    #[test]
    fn configured_type_accepts_only_exact_packaged_own_process() {
        assert_eq!(EXPECTED_PACKAGED_OWN_PROCESS, 0x210);
        assert!(has_exact_configured_service_type(
            EXPECTED_PACKAGED_OWN_PROCESS,
            EXPECTED_PACKAGED_OWN_PROCESS,
        ));

        for rejected in [
            WIN32_OWN_PROCESS,
            WIN32_SHARE_PROCESS | PACKAGED_SERVICE,
            EXPECTED_PACKAGED_OWN_PROCESS | INTERACTIVE_PROCESS,
            EXPECTED_PACKAGED_OWN_PROCESS | USER_SERVICE,
            EXPECTED_PACKAGED_OWN_PROCESS | 0x400,
        ] {
            assert!(!has_exact_configured_service_type(
                rejected,
                EXPECTED_PACKAGED_OWN_PROCESS,
            ));
        }
    }

    #[test]
    fn reported_type_accepts_only_base_or_packaged_own_process() {
        for accepted in [WIN32_OWN_PROCESS, EXPECTED_PACKAGED_OWN_PROCESS] {
            assert!(has_allowed_reported_service_type(
                accepted,
                WIN32_OWN_PROCESS,
                EXPECTED_PACKAGED_OWN_PROCESS,
            ));
        }

        for rejected in [
            WIN32_SHARE_PROCESS,
            WIN32_SHARE_PROCESS | PACKAGED_SERVICE,
            WIN32_OWN_PROCESS | INTERACTIVE_PROCESS,
            EXPECTED_PACKAGED_OWN_PROCESS | INTERACTIVE_PROCESS,
            WIN32_OWN_PROCESS | USER_SERVICE,
            EXPECTED_PACKAGED_OWN_PROCESS | USER_SERVICE,
            EXPECTED_PACKAGED_OWN_PROCESS | 0x400,
        ] {
            assert!(!has_allowed_reported_service_type(
                rejected,
                WIN32_OWN_PROCESS,
                EXPECTED_PACKAGED_OWN_PROCESS,
            ));
        }
    }
}
