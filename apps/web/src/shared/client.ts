/// 取数与会话的公共封装。**与身份无关**：两个控制台各用各的令牌，各调各的端点。
///
/// 管理端与客户端的区别只在"拿哪份令牌、调哪些路径"——JSON 编解码、错误形状、加载三态、
/// 金额与时间格式这些都是同一套，所以留在共享层。
export { apiFetch, ApiError } from './api';
export { useLoadable, Page, type Loadable } from './ui';
export { useHashRoute, yuan, when } from './routes';
