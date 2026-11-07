"use strict";

export class ApiError extends Error {
  constructor(message, status, code) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export function newRequestId() {
  if (globalThis.crypto?.randomUUID) return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function pendingKey() {
  return `pebrel.mobile.pending.v1:${location.host}:${location.pathname.split("/").slice(0, 3).join("/")}`;
}

export function readPending() {
  try {
    const value = JSON.parse(localStorage.getItem(pendingKey()) || "null");
    const values = Array.isArray(value) ? value : value ? [value] : [];
    return values.filter(
      (entry) => entry && typeof entry.requestId === "string" && typeof entry.action === "string",
    );
  } catch {
    return [];
  }
}

export function rememberPending(action, requestId) {
  const pending = readPending().filter((entry) => entry.requestId !== requestId);
  pending.push({ action, requestId, at: Date.now() });
  localStorage.setItem(pendingKey(), JSON.stringify(pending.slice(-16)));
}

export function forgetPending(requestId) {
  const pending = readPending().filter((entry) => entry.requestId !== requestId);
  if (pending.length) {
    localStorage.setItem(pendingKey(), JSON.stringify(pending));
  } else {
    localStorage.removeItem(pendingKey());
  }
}

export function errorText(error) {
  const code = error?.code;
  return {
    project_not_authorized: "这个项目没有在电脑端授权给手机。",
    conversation_not_ready: "会话身份还没有准备好，请稍后刷新。",
    conversation_identity_changed: "电脑上的原生会话已变化，请返回列表重新选择。",
    conversation_unavailable: "暂时读不到电脑上的原生会话。",
    agent_busy: "Agent 正在工作。等当前回合结束后再发送。",
    input_in_progress: "上一条消息仍在提交，请稍后刷新。",
    prompt_changed: "这个问答已经变化，请使用最新选项。",
    request_conflict: "同一个请求编号被用于了不同内容，已停止发送。",
    receipt_not_found: "电脑还没有收到这次请求的回执。",
    browser_control_disabled: "电脑端已关闭浏览器远控。",
  }[code] || error?.message || "操作没有完成。";
}

export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function pollReceipt(api, requestId, { timeout = 150_000 } = {}) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const receipt = await api(`/api/v1/receipt/${encodeURIComponent(requestId)}`, { timeout: 10_000 });
      if (!["prepared", "dispatching", "launching"].includes(receipt.status)) return receipt;
    } catch (error) {
      if (error.status === 404) return { requestId, status: "unknown" };
      if (error.status && error.status < 500) return { requestId, status: "unknown" };
    }
    await delay(1200);
  }
  return { requestId, status: "unknown" };
}
