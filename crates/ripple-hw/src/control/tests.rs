//! Tests of the control layer, against the in-memory EC.

use super::*;

#[test]
fn boost_off_restores_the_pre_boost_mode() {
    let mut stash = None;
    assert_eq!(boost_byte(0x20, true, &mut stash), 0x40);
    // a second "on" while already boosted must not overwrite the stash
    assert_eq!(boost_byte(0x40, true, &mut stash), 0x40);
    assert_eq!(boost_byte(0x40, false, &mut stash), 0x20);
    assert_eq!(stash, None);
    // no stash (restarted while boosted) → Normal
    assert_eq!(boost_byte(0x40, false, &mut stash), 0x00);
    // low nibble is preserved
    assert_eq!(boost_byte(0x23, true, &mut stash), 0x43);
    // "off" on a byte that is not boosted is a no-op
    let mut stash = Some(0x20);
    assert_eq!(boost_byte(0x20, false, &mut stash), 0x20);
    assert_eq!(stash, None);
}

#[test]
fn presets_are_validated() {
    assert!(Preset {
        pl1: 35,
        pl2: 35,
        pl4: 145,
        ctgp: 0
    }
    .validate()
    .is_ok());
    assert!(Preset {
        pl1: 80,
        pl2: 80,
        pl4: 145,
        ctgp: 20
    }
    .validate()
    .is_ok());
    assert!(Preset {
        pl1: 81,
        pl2: 81,
        pl4: 145,
        ctgp: 0
    }
    .validate()
    .is_err());
    assert!(Preset {
        pl1: 60,
        pl2: 50,
        pl4: 145,
        ctgp: 0
    }
    .validate()
    .is_err());
    assert!(Preset {
        pl1: 4,
        pl2: 40,
        pl4: 145,
        ctgp: 0
    }
    .validate()
    .is_err());
    assert!(Preset {
        pl1: 35,
        pl2: 35,
        pl4: 145,
        ctgp: 26
    }
    .validate()
    .is_err());
    // PL4 is the peak: not below PL2, not above the stock value
    let with_pl4 = |pl4| Preset {
        pl1: 35,
        pl2: 35,
        pl4,
        ctgp: 0,
    };
    assert!(with_pl4(35).validate().is_ok());
    assert!(with_pl4(145).validate().is_ok());
    assert!(with_pl4(34).validate().is_err());
    assert!(with_pl4(146).validate().is_err());
}

#[test]
fn a_preset_saved_before_pl4_existed_gets_the_stock_value() {
    let p: Preset = serde_json::from_str(r#"{"pl1":45,"pl2":45,"ctgp":0}"#).unwrap();
    assert_eq!(p.pl4, PL4_CEILING_W);
    assert!(p.validate().is_ok());
}

#[test]
fn stock_presets_come_from_the_ecs_default_slots() {
    // what the EC holds on this machine, and what the Windows app shows for each mode
    let table = [
        (OperatingMode::Office, [35, 35, 145]),
        (OperatingMode::Gaming, [55, 55, 145]),
        (OperatingMode::Turbo, [65, 65, 145]),
    ];
    let mut ec = crate::mock::MockEc::new();
    for (reg, v) in [
        (addr::OFFICE_PL1_DEFAULT, 35),
        (addr::OFFICE_PL2_DEFAULT, 35),
        (addr::OFFICE_PL4_DEFAULT, 145),
        (addr::GAMING_PL1_DEFAULT, 55),
        (addr::GAMING_PL2_DEFAULT, 55),
        (addr::GAMING_PL4_DEFAULT, 145),
        (addr::BATTERYSAVER_PL1_DEFAULT, 65),
        (addr::BATTERYSAVER_PL2_DEFAULT, 65),
        (addr::BATTERYSAVER_PL4_DEFAULT, 145),
    ] {
        ec.write(reg, v).unwrap();
    }
    // another machine would hold other values: the presets follow the EC, not a table
    let mut other = ec.clone();
    other.write(addr::GAMING_PL4_DEFAULT, 120).unwrap();
    assert_eq!(
        stock(Some(&mut other))[OperatingMode::Gaming as usize].pl4,
        120
    );
    for (mode, [pl1, pl2, pl4]) in table {
        let want = Preset {
            pl1,
            pl2,
            pl4,
            ctgp: 0,
        };
        assert_eq!(stock(Some(&mut ec))[mode as usize], want);
        // no EC, or slots that read as unset, fall back to the same values
        assert_eq!(stock(None)[mode as usize], want);
        assert_eq!(
            stock(Some(&mut crate::mock::MockEc::new()))[mode as usize],
            want
        );
        assert!(want.validate().is_ok());
    }
}

#[test]
fn apply_writes_pl4_with_the_limits() {
    let p = |pl1, pl2, pl4| Preset {
        pl1,
        pl2,
        pl4,
        ctgp: 0,
    };
    let limits = |ec: &mut crate::mock::MockEc| {
        [addr::PL1_SETTING, addr::PL2_SETTING, addr::PL4_SETTING].map(|a| ec.read(a).unwrap())
    };
    // from an unset EC (right after boot): all three go up
    let mut ec = crate::mock::MockEc::new();
    apply(&mut ec, None, p(55, 55, 145)).unwrap();
    assert_eq!(limits(&mut ec), [55, 55, 145]);
    // everything down, and a mix of up and down
    apply(&mut ec, None, p(35, 35, 100)).unwrap();
    assert_eq!(limits(&mut ec), [35, 35, 100]);
    apply(&mut ec, None, p(65, 65, 90)).unwrap();
    assert_eq!(limits(&mut ec), [65, 65, 90]);
    // an invalid preset writes nothing
    assert!(apply(&mut ec, None, p(65, 65, 60)).is_err());
    assert_eq!(limits(&mut ec), [65, 65, 90]);
    // an EC that refuses PL4 still gets PL1/PL2, and the refusal is reported, not hidden
    let mut refusing = crate::mock::MockEc::new();
    refusing.fail_on.push(addr::PL4_SETTING);
    let why = apply(&mut refusing, None, p(55, 55, 145)).unwrap();
    assert!(why.is_some_and(|w| w.contains("ec communication failure")));
    assert_eq!(refusing.read(addr::PL1_SETTING).unwrap(), 55);
    assert_eq!(refusing.read(addr::PL2_SETTING).unwrap(), 55);
}

#[test]
fn auto_boost_has_hysteresis_and_only_undoes_its_own_boost() {
    assert_eq!(auto_boost_action(None, 99.0, false, false), None);
    assert_eq!(auto_boost_action(Some(85), 85.0, false, false), Some(true));
    assert_eq!(auto_boost_action(Some(85), 80.0, true, true), None);
    assert_eq!(auto_boost_action(Some(85), 77.0, true, true), Some(false));
    // user boosted by hand: leave it alone
    assert_eq!(auto_boost_action(Some(85), 50.0, true, false), None);
}

#[test]
fn keyboard_colours_scale_to_the_ec_range() {
    assert_eq!(kb_from_8bit((255, 255, 255)), (50, 50, 50));
    assert_eq!(kb_from_8bit((0, 0, 0)), (0, 0, 0));
    assert_eq!(kb_from_8bit((255, 0, 128)), (50, 0, 25));
}

#[test]
fn only_one_process_can_follow_the_button() {
    let path = std::env::temp_dir().join(format!("ripple-follow-test-{}", std::process::id()));
    let first = follow_lock_at(&path, false).expect("free lock");
    assert!(
        follow_lock_at(&path, false).is_none(),
        "second follower must be refused"
    );
    drop(first);
    assert!(
        follow_lock_at(&path, false).is_some(),
        "released lock can be taken again"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn button_cycles_stock_profiles_and_restarts_from_anything_else() {
    use OperatingMode::*;
    // the button's LEDs go 0 → 1 → 2: Office → Gaming → Turbo → Office
    assert_eq!(
        Profile::after_button(Some(Profile::Stock(Office))),
        Profile::Stock(Gaming)
    );
    assert_eq!(
        Profile::after_button(Some(Profile::Stock(Gaming))),
        Profile::Stock(Turbo)
    );
    assert_eq!(
        Profile::after_button(Some(Profile::Stock(Turbo))),
        Profile::Stock(Office)
    );
    assert_eq!(
        Profile::after_button(Some(Profile::Custom(0))),
        Profile::Stock(Turbo)
    ); // shows as 1 light
    assert_eq!(Profile::after_button(None), Profile::Stock(Office));
    // lights: measured 0xA_ = 0 lights, 0x0_ = 1 light, 0x3_ = 2 lights
    assert_eq!(Profile::Stock(Office).mode_nibble(), 0xA);
    assert_eq!(Profile::Stock(Gaming).mode_nibble(), 0x0);
    assert_eq!(Profile::Stock(Turbo).mode_nibble(), 0x3);
}

#[test]
fn hsv_matches_the_ec_levels() {
    assert_eq!(
        Hsv {
            h: 0,
            s: 100,
            v: 100
        }
        .to_levels(),
        (50, 0, 0)
    );
    assert_eq!(
        Hsv {
            h: 120,
            s: 100,
            v: 100
        }
        .to_levels(),
        (0, 50, 0)
    );
    assert_eq!(Hsv { h: 0, s: 0, v: 100 }.to_levels(), (50, 50, 50));
    assert_eq!(
        Hsv {
            h: 0,
            s: 100,
            v: 50
        }
        .to_levels(),
        (25, 0, 0)
    );
    assert_eq!(
        Hsv {
            h: 200,
            s: 40,
            v: 0
        }
        .to_levels(),
        (0, 0, 0)
    );
    // the firmware's default colour is cyan at full brightness
    assert_eq!(
        Hsv::from_levels((0, 50, 50)),
        Hsv {
            h: 180,
            s: 100,
            v: 100
        }
    );
    for c in [(50, 0, 0), (0, 50, 25), (10, 20, 30), (50, 50, 50)] {
        let back = Hsv::from_levels(c).to_levels();
        assert!(
            back.0.abs_diff(c.0) <= 1 && back.1.abs_diff(c.1) <= 1 && back.2.abs_diff(c.2) <= 1,
            "{c:?} -> {back:?}"
        );
    }
}

#[test]
fn current_prefers_stock_over_custom() {
    struct Fake(u8, u8);
    impl EcAccess for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn read(&mut self, a: u16) -> HwResult<u8> {
            Ok(if a == addr::PL1_SETTING {
                self.0
            } else {
                self.1
            })
        }
        fn write(&mut self, _: u16, _: u8) -> HwResult<()> {
            Ok(())
        }
    }
    let st = stock(None);
    let custom = [custom_profile(Preset {
        pl1: 35,
        pl2: 35,
        pl4: 145,
        ctgp: 5,
    })]; // same limits as Office
    assert_eq!(
        current(&mut Fake(35, 35), &st, &custom, None).unwrap(),
        Some(Profile::Stock(OperatingMode::Office))
    );
    assert_eq!(
        current(
            &mut Fake(45, 45),
            &st,
            &[custom_profile(Preset {
                pl1: 45,
                pl2: 45,
                pl4: 145,
                ctgp: 0
            })],
            None
        )
        .unwrap(),
        Some(Profile::Custom(0))
    );
    assert_eq!(
        current(&mut Fake(50, 50), &st, &custom, None).unwrap(),
        None
    );
}

fn custom_profile(preset: Preset) -> CustomProfile {
    CustomProfile {
        name: "Profile 1".into(),
        base: OperatingMode::Gaming,
        preset,
    }
}

fn ec_with(pl: (u8, u8), mode_byte: u8) -> crate::mock::MockEc {
    let mut ec = crate::mock::MockEc::new();
    ec.write(addr::PL1_SETTING, pl.0).unwrap();
    ec.write(addr::PL2_SETTING, pl.1).unwrap();
    ec.write(addr::MAFAN_CONTROL_BYTE, mode_byte).unwrap();
    ec
}

#[test]
fn curves_interpolate_and_clamp() {
    let c = [20, 30, 45, 70, 100];
    assert_eq!(curve_duty(&c, 30.0), 20); // below the first anchor: flat
    assert_eq!(curve_duty(&c, 45.0), 20);
    assert_eq!(curve_duty(&c, 50.0), 25); // halfway 20 -> 30
    assert_eq!(curve_duty(&c, 75.0), 70);
    assert_eq!(curve_duty(&c, 80.0), 85);
    assert_eq!(curve_duty(&c, 120.0), 100); // above the last: flat
    assert_eq!(fan_smooth(50, 80), 80); // up at once
    assert_eq!(fan_smooth(80, 20), 78); // down by 2 per tick
    assert_eq!(fan_smooth(21, 20), 20);
}

#[test]
fn setting_a_point_keeps_the_curve_rising() {
    let mut c = [20, 30, 45, 70, 100];
    set_curve_point(&mut c, 2, 80); // up: later points follow
    assert_eq!(c, [20, 30, 80, 80, 100]);
    set_curve_point(&mut c, 2, 10); // down: earlier points follow
    assert_eq!(c, [10, 10, 10, 80, 100]);
    set_curve_point(&mut c, 4, 0);
    assert_eq!(c, [0, 0, 0, 0, 0]);
    // a curve that lets the fans stop at 85 °C is refused
    assert!(FanCurves {
        cpu: c,
        ..FanCurves::default()
    }
    .validate()
    .is_err());
    assert!(FanCurves {
        gpu: [0, 0, 0, 0, 59],
        ..FanCurves::default()
    }
    .validate()
    .is_err());
    assert!(FanCurves {
        gpu: [0, 0, 0, 0, 60],
        ..FanCurves::default()
    }
    .validate()
    .is_ok());
}

#[test]
fn fan_targets_fail_safe() {
    let cfg = FanCurves::default();
    assert_eq!(fan_targets(&cfg, Some(45.0), Some(45.0)), [20, 0]);
    assert_eq!(
        fan_targets(&cfg, Some(89.0), Some(40.0))[0],
        curve_duty(&cfg.cpu, 89.0)
    );
    assert_eq!(fan_targets(&cfg, Some(90.0), Some(40.0)), [100, 100]); // either sensor hot
    assert_eq!(fan_targets(&cfg, Some(40.0), Some(95.0)), [100, 100]);
    assert_eq!(
        fan_targets(&cfg, None, Some(65.0)),
        [curve_duty(&cfg.cpu, 65.0), curve_duty(&cfg.gpu, 65.0)]
    );
    assert_eq!(fan_targets(&cfg, None, None), [100, 100]); // blind = flat out
    assert!(FanCurves::default().validate().is_ok());
    assert!(FanCurves {
        cpu: [20, 30, 25, 70, 100],
        ..FanCurves::default()
    }
    .validate()
    .is_err());
    assert!(FanCurves {
        gpu: [0, 25, 40, 65, 101],
        ..FanCurves::default()
    }
    .validate()
    .is_err());
}

/// A mock EC with the two fan duty registers.
struct FanEc(crate::mock::MockEc, [u8; 2]);
impl EcAccess for FanEc {
    fn name(&self) -> &str {
        "fan-mock"
    }
    fn read(&mut self, a: u16) -> HwResult<u8> {
        self.0.read(a)
    }
    fn write(&mut self, a: u16, v: u8) -> HwResult<()> {
        self.0.write(a, v)
    }
    fn fan_duty(&mut self, f: u8) -> HwResult<u8> {
        Ok(self.1[usize::from(f) - 1])
    }
    fn set_fan_duty(&mut self, f: u8, d: u8) -> HwResult<()> {
        self.1[usize::from(f) - 1] = d;
        Ok(())
    }
}

#[test]
fn engine_takes_over_on_top_of_the_mode_and_hands_back() {
    let mut ec = FanEc(
        crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0xA7),
        [60, 0],
    ); // Office
    let cfg = FanCurves {
        enabled: true,
        ..FanCurves::default()
    };
    let mut eng = FanEngine {
        settle: std::time::Duration::ZERO,
        ..FanEngine::default()
    };
    eng.tick(&mut ec, &cfg, Some(65.0), Some(65.0)).unwrap();
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0xE7); // mode nibble kept, manual bit on
                                                                  // at 65 °C the curves say 45 % (CPU) and 40 % (GPU): both above what the firmware ran, so at once
    assert_eq!(ec.1, [duty_to_ec(45), duty_to_ec(40)]);
    // a hot GPU alone pushes both fans to the top at once
    eng.tick(&mut ec, &cfg, Some(60.0), Some(91.0)).unwrap();
    assert_eq!(ec.1, [200, 200]);
    // the mode button dropping bit 6 is repaired on the next tick
    ec.write(addr::MAFAN_CONTROL_BYTE, 0x37).unwrap();
    eng.tick(&mut ec, &cfg, Some(60.0), Some(60.0)).unwrap();
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x77);
    // switched off: the firmware gets the fans back, mode intact
    eng.tick(&mut ec, &FanCurves::default(), Some(60.0), Some(60.0))
        .unwrap();
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x37);
}

#[test]
fn mode_byte_only_keeps_the_manual_bit_the_engine_set() {
    use OperatingMode::*;
    let run = |cur: u8, p: Profile, on: bool| {
        let mut ec = crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, cur);
        set_mode_byte_with(&mut ec, p, on).unwrap();
        ec.read(addr::MAFAN_CONTROL_BYTE).unwrap()
    };
    // the profile being on must not create the bit: the engine never sees its own mark otherwise
    // and would take the byte for a user's boost (boot: byte 0x07, adopt writes the mode byte)
    assert_eq!(run(0x07, Profile::Stock(Gaming), true), 0x07);
    assert_eq!(run(0xA7, Profile::Stock(Turbo), true), 0x37);
    // a bit the engine set stays through a mode change
    assert_eq!(run(0xE7, Profile::Stock(Gaming), true), 0x47);
    assert_eq!(run(0x47, Profile::Stock(Turbo), true), 0x77);
    assert_eq!(run(0x77, Profile::Stock(Office), true), 0xE7);
    // profile off: plain mode change
    assert_eq!(run(0xA7, Profile::Stock(Gaming), false), 0x07);
}

#[test]
fn which_attributes_become_toggles() {
    for lightbar in [false, true] {
        assert!(!toggle_shown("ctgp_offset", lightbar)); // the profiles' business
                                                         // shown everywhere (the doubtful one is marked experimental, not hidden)
        for name in [
            "fn_lock",
            "super_key_enable",
            "ac_auto_boot",
            "usb_powershare_high",
            "touchpad_toggle_enable",
        ] {
            assert!(
                toggle_shown(name, lightbar),
                "{name} (lightbar: {lightbar})"
            );
        }
    }
    // the lightbar animations only with a lightbar
    for name in ["rainbow_animation", "breathing_in_suspend"] {
        assert!(
            !toggle_shown(name, false) && toggle_shown(name, true),
            "{name}"
        );
    }
}

#[test]
fn manual_duties_are_what_the_fans_are_told() {
    let mut ec = FanEc(
        crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x47),
        [0, 200],
    );
    // a stopped fan is 0 %, not what hwmon's reported duty says in manual mode
    assert_eq!(manual_duties(&mut ec, 0x47), Some([Some(0), Some(100)]));
    // the firmware's own curve: hwmon's number is the right one
    assert_eq!(manual_duties(&mut ec, 0xA7), None);
    // no WMI duty access: no number rather than a wrong one
    let mut plain = crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x47);
    assert_eq!(manual_duties(&mut plain, 0x47), Some([None, None]));
}

#[test]
fn the_published_profile_is_a_plain_readable_name() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("ripple-profile-test-{}", std::process::id()));
    publish_profile(&path, Some(Profile::Stock(OperatingMode::Office)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "office");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    publish_profile(&path, Some(Profile::Custom(0)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "custom");
    publish_profile(&path, None);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "unknown");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_touchpad_choice_defaults_to_on_and_survives_an_old_config() {
    assert!(serde_json::from_str::<Config>("{}").unwrap().touchpad_on);
    assert!(
        !serde_json::from_str::<Config>(r#"{"touchpad_on":false}"#)
            .unwrap()
            .touchpad_on
    );
}

#[test]
fn boost_is_the_manual_bit_with_both_fans_at_full() {
    let boosted = |byte: u8, duties: [u8; 2]| {
        boost_is_on(&mut FanEc(
            crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, byte),
            duties,
        ))
    };
    assert!(boosted(0x47, [200, 200])); // Boost
    assert!(!boosted(0x47, [60, 40])); // the custom profile's byte in Gaming, curve duties: not a boost
    assert!(!boosted(0x47, [200, 40])); // one fan at full only
    assert!(!boosted(0x07, [200, 200])); // no manual bit
    assert!(!boosted(0xE7, [200, 200])); // Office + manual is not the boost nibble
}

#[test]
fn engine_leaves_a_users_boost_alone() {
    let mut ec = FanEc(
        crate::mock::MockEc::with(addr::MAFAN_CONTROL_BYTE, 0x40),
        [200, 200],
    );
    let cfg = FanCurves {
        enabled: true,
        ..FanCurves::default()
    };
    let mut eng = FanEngine {
        settle: std::time::Duration::ZERO,
        ..FanEngine::default()
    };
    eng.tick(&mut ec, &cfg, Some(40.0), Some(40.0)).unwrap();
    assert_eq!(
        (ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), ec.1),
        (0x40, [200, 200])
    );
    eng.stop(&mut ec).unwrap(); // not ours: nothing is released
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x40);
}

#[test]
fn rainbow_is_a_sticky_bit_that_a_colour_clears() {
    let mut ec = crate::mock::MockEc::with(addr::TRIGGER_BYTE, 0x00);
    set_keyboard_rainbow(&mut ec, true).unwrap();
    assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0xA0); // rainbow + apply
    assert!(keyboard_rainbow(&mut ec).unwrap());
    set_keyboard_rgb(&mut ec, (1, 2, 3)).unwrap();
    assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0x20); // rainbow gone, apply set
    assert!(!keyboard_rainbow(&mut ec).unwrap());
    // other trigger bits survive both writes
    let mut ec = crate::mock::MockEc::with(addr::TRIGGER_BYTE, 0x01);
    set_keyboard_rainbow(&mut ec, true).unwrap();
    set_keyboard_rainbow(&mut ec, false).unwrap();
    assert_eq!(ec.read(addr::TRIGGER_BYTE).unwrap(), 0x21);
}

#[test]
fn backlight_off_and_back_on() {
    let mut ec = crate::mock::MockEc::with(KB_STATUS, 0x88); // level 4, turbo bits set
    assert_eq!(keyboard_backlight(&mut ec).unwrap(), 4);
    assert_eq!(keyboard_off(&mut ec).unwrap(), 4);
    assert_eq!(keyboard_backlight(&mut ec).unwrap(), 0);
    assert_eq!(ec.read(KB_STATUS).unwrap(), 0x18); // level 0, apply, other bits kept
    keyboard_on(&mut ec, Some(2)).unwrap();
    assert_eq!(keyboard_backlight(&mut ec).unwrap(), 2);
    keyboard_on(&mut ec, Some(4)).unwrap(); // already on: untouched
    assert_eq!(keyboard_backlight(&mut ec).unwrap(), 2);
    keyboard_off(&mut ec).unwrap();
    keyboard_on(&mut ec, None).unwrap(); // unknown level: full
    assert_eq!(keyboard_backlight(&mut ec).unwrap(), 4);
    // a power-off bit left by the EC is cleared by a write
    let mut ec = crate::mock::MockEc::with(KB_STATUS, 0x02);
    set_keyboard_backlight(&mut ec, 3).unwrap();
    assert_eq!(ec.read(KB_STATUS).unwrap(), 0x70);
}

#[test]
fn keyboard_choice_follows_the_ec_when_it_changed() {
    let saved = Hsv {
        h: 0,
        s: 100,
        v: 50,
    }; // dim red: levels (25, 0, 0)
    assert_eq!(keyboard_choice(Some(saved), Some((25, 0, 0))), Some(saved));
    assert_eq!(
        keyboard_choice(Some(saved), Some((0, 0, 50))),
        Some(Hsv::from_levels((0, 0, 50)))
    );
    assert_eq!(keyboard_choice(Some(saved), None), Some(saved));
    assert_eq!(keyboard_choice(None, None), None);
}

#[test]
fn boot_state_is_read_from_the_mode_byte() {
    use OperatingMode::*;
    let st = stock(None);
    let custom = [custom_profile(default_custom())];
    let cur = |pl, byte| current(&mut ec_with(pl, byte), &st, &custom, None).unwrap();
    // measured right after a boot on the real machine: PL regs 0/0, mode byte 0x00 (1 light)
    assert_eq!(cur((0, 0), 0x00), Some(Profile::Stock(Gaming)));
    assert_eq!(cur((0, 0), 0xA7), Some(Profile::Stock(Office)));
    assert_eq!(cur((0, 0), 0x37), Some(Profile::Stock(Turbo)));
    // measured after a Windows session in Turbo: PL regs 0/0, mode byte 0x10
    assert_eq!(cur((0, 0), 0x10), Some(Profile::Stock(Turbo)));
    assert_eq!(cur((0, 0), 0x57), Some(Profile::Stock(Turbo)));
    // fan boost (0x4_) and nibbles with no known meaning stay unknown
    assert_eq!(cur((0, 0), 0x47), None);
    assert_eq!(cur((0, 0), 0x20), None);
    // set limits that match nothing are not second-guessed by the lights
    assert_eq!(cur((50, 50), 0xA7), None);
}

#[test]
fn adopt_writes_the_limits_of_the_profile_the_lights_show() {
    let st = stock(None);
    let custom = [custom_profile(default_custom())];
    let mut ec = ec_with((0, 0), 0x07);
    let p = adopt(&mut ec, None, &st, &custom).unwrap();
    assert_eq!(p, Some(Profile::Stock(OperatingMode::Gaming)));
    assert_eq!(
        (
            ec.read(addr::PL1_SETTING).unwrap(),
            ec.read(addr::PL2_SETTING).unwrap()
        ),
        (55, 55)
    );
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x07); // lights untouched
                                                                  // limits that are already set are left alone
    let mut ec = ec_with((45, 45), 0x07);
    assert_eq!(
        adopt(&mut ec, None, &st, &custom).unwrap(),
        Some(Profile::Custom(0))
    );
    assert_eq!(ec.read(addr::PL1_SETTING).unwrap(), 45);
}

#[test]
fn a_pl4_the_ec_refuses_does_not_stop_the_limits_that_matter() {
    let mut ec = crate::mock::MockEc::new();
    ec.fail_on.push(addr::PL4_SETTING);
    let p = Preset {
        pl1: 55,
        pl2: 55,
        pl4: 145,
        ctgp: 0,
    };
    apply(&mut ec, None, p).unwrap();
    ec.fail_on.clear();
    assert_eq!(ec.read(addr::PL1_SETTING).unwrap(), 55);
    assert_eq!(ec.read(addr::PL2_SETTING).unwrap(), 55);
    assert_eq!(ec.read(addr::PL4_SETTING).unwrap(), 0);
    // PL1 and PL2 still have to take
    let mut ec = crate::mock::MockEc::new();
    ec.fail_on.push(addr::PL1_SETTING);
    assert!(apply(&mut ec, None, p).is_err());
}

#[test]
fn adopt_after_a_windows_session_in_turbo_writes_its_limits_and_keeps_the_byte() {
    // measured: limits 0/0/0 and mode byte 0x10, Windows' own Turbo
    let st = stock(None);
    let mut ec = ec_with((0, 0), 0x10);
    assert_eq!(
        adopt(&mut ec, None, &st, &[]).unwrap(),
        Some(Profile::Stock(OperatingMode::Turbo))
    );
    let limits =
        [addr::PL1_SETTING, addr::PL2_SETTING, addr::PL4_SETTING].map(|a| ec.read(a).unwrap());
    assert_eq!(limits, [65, 65, 145]); // the same numbers the Windows app shows for it
    assert_eq!(ec.read(addr::MAFAN_CONTROL_BYTE).unwrap(), 0x10); // not rewritten
}

#[test]
fn several_custom_profiles_are_told_apart() {
    let st = stock(None);
    let make = |name: &str, pl1, pl2, pl4| CustomProfile {
        name: name.into(),
        base: OperatingMode::Gaming,
        preset: Preset {
            pl1,
            pl2,
            pl4,
            ctgp: 0,
        },
    };
    let customs = [
        make("Profile 1", 45, 45, 145),
        make("Profile 2", 60, 60, 100),
        make("Profile 3", 60, 60, 120),
    ];
    let at = |pl: (u8, u8), pl4: u8| {
        let mut ec = ec_with(pl, 0x00);
        ec.write(addr::PL4_SETTING, pl4).unwrap();
        current(&mut ec, &st, &customs, None).unwrap()
    };
    assert_eq!(at((45, 45), 145), Some(Profile::Custom(0)));
    // two profiles share 60/60: the EC's PL4 says which
    assert_eq!(at((60, 60), 100), Some(Profile::Custom(1)));
    assert_eq!(at((60, 60), 120), Some(Profile::Custom(2)));
    // PL4 matching none of them: the first with these limits
    assert_eq!(at((60, 60), 0), Some(Profile::Custom(1)));
    // a stock profile wins over a custom one with the same limits
    let same_as_gaming = [make("Profile 1", 55, 55, 145)];
    let mut ec = ec_with((55, 55), 0x00);
    assert_eq!(
        current(&mut ec, &st, &same_as_gaming, None).unwrap(),
        Some(Profile::Stock(OperatingMode::Gaming))
    );
    // what to apply for each, and for an index that no longer exists
    assert_eq!(
        preset_for(&st, &customs, Profile::Custom(1)),
        customs[1].preset
    );
    assert_eq!(
        preset_for(&st, &customs, Profile::Custom(9)),
        default_custom()
    );
}

#[test]
fn the_profile_applied_last_settles_a_copy_of_a_stock_profile() {
    use OperatingMode::*;
    let st = stock(None);
    // "Profile 1" was made as a copy of Gaming: the same limits
    let customs = [CustomProfile {
        name: "Profile 1".into(),
        base: Gaming,
        preset: st[Gaming as usize],
    }];
    let mut ec = ec_with((55, 55), 0x00);
    assert_eq!(
        current(&mut ec, &st, &customs, None).unwrap(),
        Some(Profile::Stock(Gaming))
    );
    assert_eq!(
        current(&mut ec, &st, &customs, Some(0)).unwrap(),
        Some(Profile::Custom(0))
    );
    // limits that moved on (the button, Windows) beat what was applied last
    let mut moved = ec_with((35, 35), 0x00);
    assert_eq!(
        current(&mut moved, &st, &customs, Some(0)).unwrap(),
        Some(Profile::Stock(Office))
    );
    // a profile that no longer exists is ignored
    assert_eq!(
        current(&mut ec, &st, &customs, Some(7)).unwrap(),
        Some(Profile::Stock(Gaming))
    );
}

#[test]
fn the_default_fan_curves_are_valid_and_follow_the_windows_app() {
    use OperatingMode::*;
    for m in STOCK_MODES {
        assert!(default_fan_set(m).validate().is_ok(), "{m:?}");
    }
    // the app draws the same curves for Enthusiast and Overboost
    assert_eq!(default_fan_set(Gaming), default_fan_set(Turbo));
    let gaming = default_fan_set(Gaming);
    assert_eq!(gaming.cpu, [30, 35, 48, 55, 85]);
    assert_eq!(gaming.gpu, [0, 35, 48, 55, 85]); // the GPU fan stays off to 45 °C
                                                 // between the anchors the app's 70 % point at 80 °C is where the line passes
    assert_eq!(curve_duty(&gaming.cpu, 80.0), 70);
    // Balanced draws one curve for both fans, ending at 55 %: 60 here, the floor
    let office = default_fan_set(Office);
    assert_eq!(office.cpu, office.gpu);
    assert_eq!(office.cpu, [30, 35, 48, 55, FAN_MIN_AT_TOP]);
}

#[test]
fn fan_curves_belong_to_a_profile() {
    use OperatingMode::*;
    let mut cfg: Config = serde_json::from_str(
        r#"{"profiles":[{"name":"Profile 1","base":"turbo","preset":{"pl1":50,"pl2":50,"pl4":100,"ctgp":0}}]}"#,
    )
    .unwrap();
    cfg.settle();
    let (office, gaming, mine) = (
        Profile::Stock(Office),
        Profile::Stock(Gaming),
        Profile::Custom(0),
    );
    // no entry: the default of the mode, of the mode it started from for your own
    assert_eq!(cfg.fan_set(office), default_fan_set(Office));
    assert_eq!(cfg.fan_set(mine), default_fan_set(Turbo));
    // an edit belongs to that profile alone
    let mut set = cfg.fan_set(gaming);
    set_curve_point(&mut set.cpu, 2, 60);
    cfg.set_fan_set(gaming, set);
    assert_eq!(cfg.fan_set(gaming), set);
    assert_eq!(cfg.fan_set(office), default_fan_set(Office));
    assert_eq!(cfg.fan_set(mine), default_fan_set(Turbo));
    assert_eq!(cfg.profile_key(mine), "Profile 1");
    // putting the default back leaves nothing stored
    cfg.set_fan_set(gaming, default_fan_set(Gaming));
    assert!(cfg.fan_sets.is_empty());
    // what the engine gets: the profile's curves and the one on/off switch
    cfg.fan.enabled = true;
    let curves = cfg.fan_curves(mine);
    assert!(curves.enabled);
    assert_eq!(curves.cpu, default_fan_set(Turbo).cpu);
    // a profile that is gone falls back to Gaming
    assert_eq!(cfg.fan_set(Profile::Custom(9)), default_fan_set(Gaming));
}

#[test]
fn an_older_fan_config_keeps_its_switch_and_a_broken_set_is_dropped() {
    // older versions kept one pair of curves next to the switch: only the switch counts now
    let mut cfg: Config = serde_json::from_str(
        r#"{"fan":{"enabled":true,"cpu":[20,30,45,70,100],"gpu":[0,25,40,65,100]},
            "fan_sets":{"gaming":{"cpu":[1,2,3,4,5],"gpu":[0,0,0,0,100]},
                        "turbo":{"cpu":[30,35,48,55,85],"gpu":[0,35,48,55,90]}}}"#,
    )
    .unwrap();
    cfg.settle();
    assert!(cfg.fan.enabled);
    // the first is under the floor at 85 °C and dropped, the second kept
    assert_eq!(cfg.fan_sets.keys().collect::<Vec<_>>(), ["turbo"]);
    assert!(!serde_json::to_string(&cfg)
        .unwrap()
        .contains("[20,30,45,70,100]"));
}

#[test]
fn profiles_are_named_in_order_and_gaps_are_reused() {
    let named = |names: &[&str]| -> Vec<CustomProfile> {
        names
            .iter()
            .map(|n| CustomProfile {
                name: (*n).into(),
                base: OperatingMode::Office,
                preset: default_custom(),
            })
            .collect()
    };
    assert_eq!(next_profile_name(&[]), "Profile 1");
    assert_eq!(next_profile_name(&named(&["Profile 1"])), "Profile 2");
    assert_eq!(
        next_profile_name(&named(&["Profile 1", "Profile 3"])),
        "Profile 2"
    );
    assert_eq!(next_profile_name(&named(&["Mine"])), "Profile 1");
}

#[test]
fn the_single_custom_preset_of_older_versions_becomes_profile_1() {
    let load = |json: &str| {
        let mut cfg: Config = serde_json::from_str(json).unwrap();
        cfg.settle();
        cfg
    };
    // changed from the default: kept, as the first profile, based on Gaming
    let cfg = load(r#"{"custom":{"pl1":50,"pl2":60,"ctgp":5}}"#);
    assert_eq!(cfg.profiles.len(), 1);
    assert_eq!(cfg.profiles[0].name, "Profile 1");
    assert_eq!(cfg.profiles[0].base, OperatingMode::Gaming);
    assert_eq!(
        cfg.profiles[0].preset,
        Preset {
            pl1: 50,
            pl2: 60,
            pl4: PL4_CEILING_W,
            ctgp: 5
        }
    );
    // never customised: nothing to keep
    let untouched = serde_json::to_string(&default_custom()).unwrap();
    assert!(load(&format!(r#"{{"custom":{untouched}}}"#))
        .profiles
        .is_empty());
    // the new format wins over a leftover old key, and an invalid profile is dropped
    let cfg = load(
        r#"{"custom":{"pl1":50,"pl2":60,"ctgp":0},"profiles":[
            {"name":"A","base":"turbo","preset":{"pl1":40,"pl2":40,"pl4":100,"ctgp":0}},
            {"name":"Bad","base":"office","preset":{"pl1":90,"pl2":90,"pl4":100,"ctgp":0}}]}"#,
    );
    assert_eq!(cfg.profiles.len(), 1);
    assert_eq!(cfg.profiles[0].name, "A");
    assert_eq!(cfg.profiles[0].base, OperatingMode::Turbo);
    // the old key is never written back
    assert!(!serde_json::to_string(&cfg).unwrap().contains("\"custom\""));
}

fn temp_config(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ripple-config-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.join("config.json")
}

#[test]
fn a_profile_lives_through_new_edit_curve_and_delete() {
    let path = temp_config("life");
    let gaming = default_custom();
    Config::update_at(&path, |c| {
        let name = next_profile_name(&c.profiles);
        c.profiles.push(CustomProfile {
            name,
            base: OperatingMode::Gaming,
            preset: gaming,
        });
        Ok(())
    })
    .unwrap();
    let mut cfg = Config::load_from(&path);
    assert_eq!(cfg.profiles.len(), 1);
    let p = Profile::Custom(0);
    let mut set = cfg.fan_set(p);
    set_curve_point(&mut set.cpu, 2, 70);
    Config::update_at(&path, |c| {
        c.set_fan_set(p, set);
        Ok(())
    })
    .unwrap();
    cfg = Config::load_from(&path);
    assert_eq!(cfg.fan_set(p).cpu, set.cpu);
    assert_eq!(cfg.fan_sets.len(), 1, "only a changed curve is stored");
    Config::update_at(&path, |c| {
        let gone = c.profiles.remove(0);
        c.fan_sets.remove(&gone.name);
        Ok(())
    })
    .unwrap();
    cfg = Config::load_from(&path);
    assert!(cfg.profiles.is_empty() && cfg.fan_sets.is_empty());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn editors_in_parallel_lose_no_update() {
    let path = temp_config("race");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..5 {
                    Config::update_at(&path, |c| {
                        let name = next_profile_name(&c.profiles);
                        c.profiles.push(CustomProfile {
                            name,
                            base: OperatingMode::Gaming,
                            preset: default_custom(),
                        });
                        Ok(())
                    })
                    .unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(Config::load_from(&path).profiles.len(), 40);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_damaged_file_is_left_alone_and_a_failed_edit_writes_nothing() {
    let path = temp_config("damaged");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let err = Config::update_at(&path, |c| {
        c.touchpad_on = false;
        Ok(())
    })
    .unwrap_err();
    assert!(err.to_string().contains("damaged"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    // readers still get the defaults
    assert!(Config::load_from(&path).touchpad_on);
    // an edit that fails after changing something is not saved
    std::fs::write(&path, "{}").unwrap();
    let r: HwResult<()> = Config::update_at(&path, |c| {
        c.touchpad_on = false;
        Err("no".to_string().into())
    });
    assert!(r.is_err());
    assert!(Config::load_from(&path).touchpad_on);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_touchpad_switch_is_off_when_the_pad_is_silenced_as_well_as_when_it_is_powered_down() {
    assert_eq!(touchpad_state(None, None), None); // no touchpad
    assert_eq!(touchpad_state(Some(true), Some(true)), Some(true));
    assert_eq!(touchpad_state(Some(true), None), Some(true)); // surface not readable: assume reporting
    assert_eq!(touchpad_state(Some(true), Some(false)), Some(false)); // silenced by the double tap
    assert_eq!(touchpad_state(Some(false), None), Some(false)); // powered down
    assert_eq!(touchpad_state(Some(false), Some(false)), Some(false));
}
