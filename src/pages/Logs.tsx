/**
 * 审计日志页面
 *
 * Phase 1: 骨架占位
 * Phase 后续: 实现审计日志查看器、筛选、搜索、导出
 */
function Logs() {
  return (
    <div className="page-container">
      <header className="page-header">
        <h1>审计日志</h1>
        <p className="page-subtitle">所有工具调用的完整记录</p>
      </header>

      <div className="section-card">
        <div className="logs-toolbar">
          <div className="filter-group">
            <span>筛选：</span>
            <button className="filter-btn active">全部</button>
            <button className="filter-btn">SAFE</button>
            <button className="filter-btn">LOW</button>
            <button className="filter-btn">MEDIUM</button>
            <button className="filter-btn">HIGH</button>
          </div>
        </div>

        <div className="logs-table">
          <div className="logs-header">
            <span className="col-time">时间</span>
            <span className="col-tool">工具</span>
            <span className="col-risk">风险</span>
            <span className="col-result">结果</span>
          </div>
          <div className="logs-body">
            <p className="empty-state">
              暂无日志记录
              <br />
              <small>Phase 2 实现完整审计日志功能</small>
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

export default Logs;
