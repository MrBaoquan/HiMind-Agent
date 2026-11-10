pub(crate) mod manager;
pub(crate) mod ownership;
pub(crate) mod policy;
pub(crate) mod remote;
pub(crate) mod types;

// 通知通道属于审批的出口，但独立成模块：它同时服务运行失败等其他事件。
pub(crate) use manager::append_event_log;
