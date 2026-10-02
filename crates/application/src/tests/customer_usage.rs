use seeai_domain::JobState;

use super::*;

/// 对客状态是**收敛过的值**：内部 Job 状态不进对客响应，而分流的唯一判据是"结果定了没有"。
///
/// 对账中（`reconciliation_required`）结果还没定，必须显示"处理中"（Spec C9）——并进"未产出"会让
/// 客户以为这一笔已经失败，而它还可能补回成功；取消是终态，也不能并进"处理中"。
#[test]
fn every_job_state_maps_to_the_contracted_consumer_status() {
    let cases = [
        (JobState::Accepted, CustomerUsageStatus::Pending),
        (JobState::Leased, CustomerUsageStatus::Pending),
        (JobState::Submitting, CustomerUsageStatus::Pending),
        (
            JobState::ReconciliationRequired,
            CustomerUsageStatus::Pending,
        ),
        (JobState::Succeeded, CustomerUsageStatus::Succeeded),
        (JobState::Failed, CustomerUsageStatus::Failed),
        (JobState::Canceled, CustomerUsageStatus::Canceled),
    ];
    for (state, expected) in cases {
        assert_eq!(
            customer_usage_status(state),
            expected,
            "内部状态 {state:?} 的对客取值错了"
        );
    }
}
