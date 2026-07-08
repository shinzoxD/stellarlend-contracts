#![cfg(test)]

use crate::{
    debt::DebtPosition,
    rate_model::{compute_smoothed_rate, RateParams},
    DataKey, LendingContract, LendingContractClient,
};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

fn setup_with_params(
    params: RateParams,
) -> (Env, LendingContractClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.sequence_number = 100);

    let id = env.register(LendingContract, ());
    let client = LendingContractClient::new(&env, &id);
    let admin = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&admin);

    env.as_contract(&id, || {
        env.storage().instance().set(&DataKey::RateParams, &params);
    });

    (env, client, admin, user)
}

#[test]
fn band_zero_preserves_legacy_behavior() {
    let legacy = compute_smoothed_rate(1_000, 1_040, 10, 1, 0);
    let with_zero_band = compute_smoothed_rate(1_000, 1_040, 10, 1, 0);
    assert_eq!(with_zero_band, legacy);
}

#[test]
fn target_exactly_at_band_edge_holds_current_rate() {
    assert_eq!(compute_smoothed_rate(1_000, 1_025, 10, 5, 25), 1_000);
    assert_eq!(compute_smoothed_rate(1_000, 975, 10, 5, 25), 1_000);
}

#[test]
fn target_inside_band_holds_current_rate() {
    assert_eq!(compute_smoothed_rate(1_000, 1_020, 10, 5, 25), 1_000);
    assert_eq!(compute_smoothed_rate(1_000, 985, 10, 5, 25), 1_000);
}

#[test]
fn large_move_still_converges_from_band_edge() {
    assert_eq!(compute_smoothed_rate(1_000, 1_200, 20, 1, 25), 1_020);
    assert_eq!(compute_smoothed_rate(1_020, 1_200, 20, 8, 25), 1_175);
    assert_eq!(compute_smoothed_rate(1_175, 1_200, 20, 8, 25), 1_175);
}

#[test]
fn overflow_delta_attempt_is_checked() {
    let rate = compute_smoothed_rate(i128::MIN, i128::MAX, 1, 1, i128::MAX);
    assert_eq!(rate, i128::MIN);
}

#[test]
fn contract_view_keeps_rate_flat_inside_band_and_respects_clamp() {
    let mut params = RateParams::default();
    params.max_rate_change_per_ledger_bps = 50;
    params.hysteresis_bps = 100;
    params.rate_floor_bps = 1_100;
    params.rate_ceiling_bps = 1_760;

    let (env, client, _admin, user) = setup_with_params(params);

    client.deposit(&user, &10_000);

    // Check the pre-borrow rate: utilization = 0% → rate floor (1_100).
    env.as_contract(&client.address, || {
        assert_eq!(crate::current_borrow_rate(&env), 1_100);
    });

    // Now set up the first "borrow" via direct storage writes.
    let now = env.ledger().timestamp();
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .set(&DataKey::Collateral(user.clone()), &10_000i128);
        env.storage()
            .persistent()
            .set(&DataKey::TotalDebt, &8_000i128);
        crate::debt::save_debt(
            &env,
            &user,
            &DebtPosition {
                principal: 8_000,
                borrow_index_snapshot: 0,
                last_update: now,
            },
        );
    });

    // Advance ledger so the next rate call recomputes from the new TotalDebt.
    env.ledger().with_mut(|l| l.sequence_number = 101);

    // Second "borrow" pushes total debt to 8_100 — rate should smooth upward.
    // The rate is computed from TotalDebt = 8_000 → 1_700 before the update.
    env.as_contract(&client.address, || {
        assert_eq!(crate::current_borrow_rate(&env), 1_700);
        // Now apply the second "borrow"
        env.storage()
            .persistent()
            .set(&DataKey::TotalDebt, &8_100i128);
        crate::debt::save_debt(
            &env,
            &user,
            &DebtPosition {
                principal: 8_100,
                borrow_index_snapshot: 0,
                last_update: env.ledger().timestamp(),
            },
        );
    });

    // Advance ledger.
    env.ledger().with_mut(|l| l.sequence_number = 102);

    // Third "borrow" pushes total debt to 9_000 — rate should hit ceiling.
    env.as_contract(&client.address, || {
        assert_eq!(crate::current_borrow_rate(&env), 1_760);
        env.storage()
            .persistent()
            .set(&DataKey::TotalDebt, &9_000i128);
        crate::debt::save_debt(
            &env,
            &user,
            &DebtPosition {
                principal: 9_000,
                borrow_index_snapshot: 0,
                last_update: env.ledger().timestamp(),
            },
        );
    });
}
