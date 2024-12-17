use assert_matches::assert_matches;
use near_async::time::Duration;
use near_o11y::testonly::init_test_logger;
use near_primitives::action::{Action, FunctionCallAction};
use near_primitives::errors::{
    ActionError, ActionErrorKind, FunctionCallError, InvalidTxError, ReceiptValidationError,
    TxExecutionError,
};
use near_primitives::hash::CryptoHash;
use near_primitives::receipt::{ActionReceipt, Receipt, ReceiptEnum, ReceiptV0};
use near_primitives::test_utils::create_user_test_signer;
use near_primitives::transaction::SignedTransaction;
use near_primitives::types::AccountId;
use near_primitives::views::FinalExecutionStatus;

use crate::test_loop::env::TestLoopEnv;
use crate::test_loop::utils::setups::standard_setup_1;
use crate::test_loop::utils::transactions::{execute_tx, get_shared_block_hash, run_tx};
use crate::test_loop::utils::TGAS;

/// Generating receipts larger than the size limit should cause the transaction to fail.
#[test]
fn slow_test_max_receipt_size() {
    init_test_logger();
    let mut env: TestLoopEnv = standard_setup_1();

    let account0: AccountId = "account0".parse().unwrap();
    let account0_signer = &create_user_test_signer(&account0).into();
    let rpc_id = "account4".parse().unwrap();

    // We can't test receipt limit by submitting large transactions because we hit the transaction size limit
    // before hitting the receipt size limit.
    let large_tx = SignedTransaction::deploy_contract(
        100,
        &account0,
        vec![0u8; 2_000_000],
        account0_signer,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    let large_tx_exec_res =
        execute_tx(&mut env.test_loop, &rpc_id, large_tx, &env.datas, Duration::seconds(5));
    assert_matches!(large_tx_exec_res, Err(InvalidTxError::TransactionSizeExceeded { .. }));

    // Let's test it by running a contract that generates a large receipt.
    let deploy_contract_tx = SignedTransaction::deploy_contract(
        101,
        &account0,
        near_test_contracts::rs_contract().into(),
        &account0_signer,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, &rpc_id, deploy_contract_tx, &env.datas, Duration::seconds(5));

    // Calling generate_large_receipt({"account_id": "account0", "method_name": "noop", "total_args_size": 3000000})
    // will generate a receipt that has ~3_000_000 bytes. It'll be a single receipt with multiple FunctionCall actions.
    // 3MB is still under the limit, so this should succeed.
    let large_receipt_tx = SignedTransaction::call(
        102,
        account0.clone(),
        account0.clone(),
        &account0_signer,
        0,
        "generate_large_receipt".into(),
        r#"{"account_id": "account0", "method_name": "noop", "total_args_size": 3000000}"#.into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, &rpc_id, large_receipt_tx, &env.datas, Duration::seconds(5));

    // Generating a receipt that is 5 MB should fail, it's above the receipt size limit.
    let too_large_receipt_tx = SignedTransaction::call(
        103,
        account0.clone(),
        account0.clone(),
        &account0_signer,
        0,
        "generate_large_receipt".into(),
        r#"{"account_id": "account0", "method_name": "noop", "total_args_size": 5000000}"#.into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    let too_large_receipt_tx_exec_res = execute_tx(
        &mut env.test_loop,
        &rpc_id,
        too_large_receipt_tx,
        &env.datas,
        Duration::seconds(5),
    )
    .unwrap();

    match too_large_receipt_tx_exec_res.status {
        FinalExecutionStatus::Failure(TxExecutionError::ActionError(action_error)) => {
            match action_error.kind {
                ActionErrorKind::NewReceiptValidationError(
                    ReceiptValidationError::ReceiptSizeExceeded { .. },
                ) => {
                    // Ok, got the expected error
                }
                _ => panic!("Expected ReceiptSizeExceeded error, got: {:?}", action_error),
            }
        }
        _ => panic!(
            "Expected FinalExecutionStatus::Failure, got: {:?}",
            too_large_receipt_tx_exec_res
        ),
    };

    // The runtime shouldn't die when it encounters a large receipt.
    // Make sure that everything still works.

    // Calling sum_n(5) should return 10.
    // 1 + 2 + 3 + 4 = 10
    let sum_4_tx = SignedTransaction::call(
        104,
        account0.clone(),
        account0,
        &account0_signer,
        0,
        "sum_n".into(),
        5_u64.to_le_bytes().to_vec(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    let sum_4_res = run_tx(&mut env.test_loop, &rpc_id, sum_4_tx, &env.datas, Duration::seconds(5));
    assert_eq!(sum_4_res, 10u64.to_le_bytes().to_vec());

    env.shutdown_and_drain_remaining_events(Duration::seconds(20));
}

// A function call will generate a new receipt. Size of this receipt will be equal to
// `max_receipt_size`, it'll pass validation, but then `output_data_receivers` will be modified and
// the receipt's size will go above max_receipt_size. The receipt should be rejected, but currently
// isn't because of a bug (See https://github.com/near/nearcore/issues/12606)
// Runtime shouldn't die when it encounters a receipt with size above `max_receipt_size`.
#[test]
fn test_max_receipt_size_promise_return() {
    init_test_logger();
    let mut env: TestLoopEnv = standard_setup_1();

    let account: AccountId = "account0".parse().unwrap();
    let account_signer = &create_user_test_signer(&account).into();

    // Deploy the test contract
    let deploy_contract_tx = SignedTransaction::deploy_contract(
        101,
        &account,
        near_test_contracts::rs_contract().into(),
        &account_signer,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, deploy_contract_tx, &env.datas, Duration::seconds(5));

    // User calls a contract method
    // Contract method creates a DAG with two promises: [A -then-> B]
    // When promise A is executed, it creates a third promise - `C` and does a `promise_return`.
    // The DAG changes to: [C ->then-> B]
    // The receipt for promise C is a maximum size receipt.
    // Adding the `output_data_receivers` to C's receipt makes it go over the size limit.
    let base_receipt_template = Receipt::V0(ReceiptV0 {
        predecessor_id: account.clone(),
        receiver_id: account.clone(),
        receipt_id: CryptoHash::default(),
        receipt: ReceiptEnum::Action(ActionReceipt {
            signer_id: account.clone(),
            signer_public_key: account_signer.public_key().into(),
            gas_price: 0,
            output_data_receivers: vec![],
            input_data_ids: vec![],
            actions: vec![Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: "noop".into(),
                args: vec![],
                gas: 0,
                deposit: 0,
            }))],
        }),
    });
    let base_receipt_size = borsh::object_length(&base_receipt_template).unwrap();
    let max_receipt_size = 4_194_304;
    let args_size = max_receipt_size - base_receipt_size;

    // Call the contract
    let large_receipt_tx = SignedTransaction::call(
        102,
        account.clone(),
        account.clone(),
        &account_signer,
        0,
        "max_receipt_size_promise_return_method1".into(),
        format!("{{\"args_size\": {}}}", args_size).into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, large_receipt_tx, &env.datas, Duration::seconds(5));

    // Make sure that the last promise in the DAG was called
    let assert_test_completed = SignedTransaction::call(
        103,
        account.clone(),
        account,
        &account_signer,
        0,
        "assert_test_completed".into(),
        "".into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, assert_test_completed, &env.datas, Duration::seconds(5));

    env.shutdown_and_drain_remaining_events(Duration::seconds(20));
}

/// Return a value that is as large as max_receipt_size. The value will be wrapped in a data receipt
/// and the data receipt will be bigger than max_receipt_size. The receipt should be rejected, but
/// currently isn't because of a bug (See https://github.com/near/nearcore/issues/12606)
/// Creates the following promise DAG:
/// A[self.return_large_value()] -then-> B[self.mark_test_completed()]
#[test]
fn test_max_receipt_size_value_return() {
    init_test_logger();
    let mut env: TestLoopEnv = standard_setup_1();

    let account: AccountId = "account0".parse().unwrap();
    let account_signer = &create_user_test_signer(&account).into();

    // Deploy the test contract
    let deploy_contract_tx = SignedTransaction::deploy_contract(
        101,
        &account,
        near_test_contracts::rs_contract().into(),
        &account_signer,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, deploy_contract_tx, &env.datas, Duration::seconds(5));

    let max_receipt_size = 4_194_304;

    // Call the contract
    let large_receipt_tx = SignedTransaction::call(
        102,
        account.clone(),
        account.clone(),
        &account_signer,
        0,
        "max_receipt_size_value_return_method".into(),
        format!("{{\"value_size\": {}}}", max_receipt_size).into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, large_receipt_tx, &env.datas, Duration::seconds(5));

    // Make sure that the last promise in the DAG was called
    let assert_test_completed = SignedTransaction::call(
        103,
        account.clone(),
        account,
        &account_signer,
        0,
        "assert_test_completed".into(),
        "".into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, assert_test_completed, &env.datas, Duration::seconds(5));

    env.shutdown_and_drain_remaining_events(Duration::seconds(20));
}

/// Yielding produces a new action receipt, resuming produces a new data receipt.
/// Make sure that the size of receipts produced by yield/resume is limited to below `max_receipt_size`.
#[test]
fn test_max_receipt_size_yield_resume() {
    init_test_logger();
    let mut env: TestLoopEnv = standard_setup_1();

    let account: AccountId = "account0".parse().unwrap();
    let account_signer = &create_user_test_signer(&account).into();

    // Deploy the test contract
    let deploy_contract_tx = SignedTransaction::deploy_contract(
        101,
        &account,
        near_test_contracts::rs_contract().into(),
        &account_signer,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    run_tx(&mut env.test_loop, deploy_contract_tx, &env.datas, Duration::seconds(50));

    let max_receipt_size = 4_194_304;

    // Perform a yield which creates a receipt that is larger than the max_receipt_size.
    // It should be rejected because of the receipt size limit.
    let yield_receipt_tx = SignedTransaction::call(
        102,
        account.clone(),
        account.clone(),
        &account_signer,
        0,
        "yield_with_large_args".into(),
        format!("{{\"args_size\": {}}}", max_receipt_size).into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    let yield_receipt_res =
        execute_tx(&mut env.test_loop, yield_receipt_tx, &env.datas, Duration::seconds(10))
            .unwrap();

    let expected_yield_status =
        FinalExecutionStatus::Failure(TxExecutionError::ActionError(ActionError {
            index: Some(0),
            kind: ActionErrorKind::NewReceiptValidationError(
                ReceiptValidationError::ReceiptSizeExceeded {
                    size: 4194503,
                    limit: max_receipt_size,
                },
            ),
        }));
    assert_eq!(yield_receipt_res.status, expected_yield_status);

    // Perform a resume which would create a large data receipt.
    // It fails because the max payload size is 1024.
    // Definitely not exceeding max_receipt_size.
    let resume_receipt_tx = SignedTransaction::call(
        103,
        account.clone(),
        account,
        &account_signer,
        0,
        "resume_with_large_payload".into(),
        format!("{{\"payload_size\": {}}}", 2000).into(),
        300 * TGAS,
        get_shared_block_hash(&env.datas, &env.test_loop),
    );
    let resume_receipt_res =
        execute_tx(&mut env.test_loop, resume_receipt_tx, &env.datas, Duration::seconds(5))
            .unwrap();

    let expected_resume_status =
        FinalExecutionStatus::Failure(TxExecutionError::ActionError(ActionError {
            index: Some(0),
            kind: ActionErrorKind::FunctionCallError(FunctionCallError::ExecutionError(
                "Yield resume payload is 2000 bytes which exceeds the 1024 byte limit".to_string(),
            )),
        }));
    assert_eq!(resume_receipt_res.status, expected_resume_status);

    env.shutdown_and_drain_remaining_events(Duration::seconds(20));
}
