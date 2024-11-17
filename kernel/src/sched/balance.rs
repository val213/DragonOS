use alloc::sync::Arc;
use system_error::SystemError;
use unified_init::macros::unified_init;
use crate::{exception::softirq::{softirq_vectors, SoftirqNumber}, init::initcall::INITCALL_SUBSYS};
use super::fair::DoRebalanceSoftirq;

#[unified_init(INITCALL_SUBSYS)]
/// 注册多核负载均衡的软中断
fn register_rebalance_softirq()->Result<(), SystemError>{
    let sched_softirq = Arc::new(DoRebalanceSoftirq::new());
    softirq_vectors().register_softirq(SoftirqNumber::SCHED, sched_softirq)
    .expect("Failed to register rebalance softirq");
    Ok(())
}