"use strict";

import {
  ApiError,
  delay,
  errorText,
  forgetPending,
  newRequestId,
  pollReceipt,
  readPending,
  rememberPending,
} from "/transport.js";
import {
  agentLabel,
  projectName,
  renderProjects,
  setConnection,
  stateLabel,
} from "/presentation.js";

const $ = (id) => document.getElementById(id);
const ui = {
  computerName: $("computer-name"),
  connection: $("connection-state"),
  offline: $("offline-card"),
  reconnect: $("reconnect"),
  empty: $("empty-state"),
  focusNew: $("focus-new"),
  newToggle: $("new-toggle"),
  composer: $("composer-card"),
  collapseNew: $("collapse-new"),
  newForm: $("new-session-form"),
  projectSelect: $("project-select"),
  newPrompt: $("new-prompt"),
  newSubmit: $("new-submit"),
  sessions: $("sessions-card"),
  sessionList: $("session-list"),
  noSessions: $("no-sessions"),
  refresh: $("refresh"),
  conversation: $("conversation-card"),
  closeConversation: $("close-conversation"),
  conversationTitle: $("conversation-title"),
  conversationMeta: $("conversation-meta"),
  stop: $("stop"),
  syncNote: $("sync-note"),
  messages: $("message-list"),
  prompt: $("prompt-card"),
  sendForm: $("send-form"),
  sendText: $("send-text"),
  sendSubmit: $("send-submit"),
  projects: $("projects-card"),
  projectList: $("project-list"),
  toast: $("toast"),
};

let snapshot = null;
let selected = null;
let selectedRead = null;
let selectedRevision = null;
let canSend = false;
let writeBusy = false;
let stopArmed = false;
let stopTimer = 0;
let recoveringPending = false;
let nextPendingCheck = 0;
let toastTimer = 0;
let reconnectDelay = 1000;
let readTimer = 0;
let readSequence = 0;
let stateTimer = 0;
let stateSequence = 0;
let conversationSequence = 0;

function showToast(message, error = false) {
  clearTimeout(toastTimer);
  ui.toast.textContent = message;
  ui.toast.classList.toggle("error", error);
  ui.toast.classList.remove("hidden");
  toastTimer = setTimeout(() => ui.toast.classList.add("hidden"), 4200);
}

async function api(path, options = {}) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), options.timeout ?? 20_000);
  try {
    const response = await fetch(path, {
      credentials: "same-origin",
      cache: "no-store",
      ...options,
      signal: controller.signal,
      headers: options.body
        ? { "content-type": "application/json", ...(options.headers || {}) }
        : options.headers,
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new ApiError(
        data?.error?.message || data?.reason || `HTTP ${response.status}`,
        response.status,
        data?.error?.code || data?.reason || "request_failed",
      );
    }
    setConnection(ui, true, "已连接");
    reconnectDelay = 1000;
    return data;
  } catch (error) {
    if (error.name === "AbortError") {
      throw new ApiError("电脑响应超时。", 0, "timeout");
    }
    if (!(error instanceof ApiError)) {
      throw new ApiError("无法连接电脑。任务可能仍在运行。", 0, "offline");
    }
    if (error.status >= 500 || error.status === 0) setConnection(ui, false, "连接中断");
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

function renderSessions() {
  const sessions = snapshot?.sessions || [];
  ui.sessionList.replaceChildren();
  ui.noSessions.classList.toggle("hidden", sessions.length !== 0);
  ui.empty.classList.toggle("hidden", sessions.length !== 0 || writeBusy);
  for (const session of sessions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "session-item";
    button.classList.toggle("active", selected?.paneId === session.paneId && selected?.windowId === session.windowId);
    const mark = document.createElement("span");
    mark.className = "agent-mark";
    mark.textContent = agentLabel(session.kind);
    const copy = document.createElement("span");
    copy.className = "item-copy";
    const title = document.createElement("strong");
    title.textContent = session.title || agentLabel(session.kind);
    const meta = document.createElement("span");
    meta.textContent = `${projectName(snapshot, session.projectId)} · ${session.kind}`;
    copy.append(title, meta);
    const status = document.createElement("span");
    status.className = `state ${String(session.state || "unknown").toLowerCase()}`;
    status.textContent = stateLabel(session.state);
    button.append(mark, copy, status);
    button.addEventListener("click", () => openConversation(session));
    ui.sessionList.append(button);
  }
}

function renderSnapshot(next, { preserveSelection = true } = {}) {
  snapshot = next;
  ui.computerName.textContent = next.deviceName || "Pebrel Computer";
  renderProjects(ui, snapshot?.projects || [], writeBusy);
  renderSessions();
  if (preserveSelection && selected) {
    const current = next.sessions.find(
      (item) => item.windowId === selected.windowId && item.paneId === selected.paneId,
    );
    if (current) {
      if (current.sessionId !== selected.sessionId || current.kind !== selected.kind) {
        closeConversation(true);
        showToast("电脑上的会话已变化，请重新选择。", true);
        return;
      }
      selected = current;
      updateControls();
    }
    else closeConversation(true);
  }
}

async function refreshState({ silent = false } = {}) {
  clearTimeout(stateTimer);
  const sequence = ++stateSequence;
  try {
    const next = await api("/api/v1/state", { timeout: 12_000 });
    if (sequence !== stateSequence) return null;
    renderSnapshot(next);
    if (!recoveringPending && Date.now() >= nextPendingCheck && readPending().length) {
      recoveringPending = true;
      nextPendingCheck = Date.now() + 30_000;
      recoverPending().catch(() => {}).finally(() => { recoveringPending = false; });
    }
    if (!selected) ui.offline.classList.add("hidden");
    stateTimer = setTimeout(() => refreshState({ silent: true }).catch(() => {}), 5000);
    return next;
  } catch (error) {
    if (sequence !== stateSequence) return null;
    if (!silent) showToast(errorText(error), true);
    setConnection(ui, false, "电脑离线");
    stateTimer = setTimeout(() => refreshState({ silent: true }).catch(() => {}), reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 30_000);
    throw error;
  }
}

function setWriteBusy(value, label = "") {
  writeBusy = value;
  ui.newSubmit.disabled = value || !snapshot?.projects?.length;
  ui.newSubmit.textContent = value && label ? label : "在电脑上启动";
  updateControls();
}

function canStopState(state) {
  return !["idle", "finished"].includes(String(state || "unknown").toLowerCase());
}

function updateControls() {
  ui.sendSubmit.disabled = writeBusy || !canSend;
  ui.stop.disabled = writeBusy || !selectedRead || !selected || !canStopState(selected.state);
}

async function performWrite(path, body, action) {
  if (writeBusy) return null;
  const requestId = newRequestId();
  rememberPending(action, requestId);
  setWriteBusy(true, action === "new" ? "正在启动…" : "正在发送…");
  try {
    let response = await api(path, {
      method: "POST",
      body: JSON.stringify({ ...body, requestId }),
      timeout: 35_000,
    });
    if (action !== "new" && ["launching", "dispatching", "prepared"].includes(response.status)) {
      response = await pollReceipt(api, requestId);
    }
    if (!["unknown", "launching", "dispatching", "prepared"].includes(response.status)) forgetPending(requestId);
    if (["rejected", "failed"].includes(response.status)) {
      throw new ApiError("电脑未接受这次操作，请刷新会话后核对。", 409, "request_rejected");
    }
    return response;
  } catch (error) {
    if (error.status && error.status >= 400 && error.status < 500) forgetPending(requestId);
    throw error;
  } finally {
    setWriteBusy(false);
  }
}

function renderMessages(messages, unchanged = false) {
  if (unchanged) return;
  const follow = ui.messages.scrollHeight - ui.messages.clientHeight - ui.messages.scrollTop < 72;
  const scrollTop = ui.messages.scrollTop;
  ui.messages.replaceChildren();
  for (const message of messages || []) {
    const article = document.createElement("article");
    article.className = `message ${String(message.role || "assistant").toLowerCase()}`;
    const role = document.createElement("span");
    role.className = "role";
    role.textContent = message.role === "user"
      ? "你"
      : message.role === "tool"
        ? "工具"
        : (message.name || agentLabel(selected?.kind));
    const bubble = document.createElement("div");
    bubble.className = "bubble";
    const text = document.createElement("pre");
    text.textContent = message.text || "";
    bubble.append(text);
    if (message.detail) {
      const detail = document.createElement("div");
      detail.className = "detail";
      detail.textContent = message.detail;
      bubble.append(detail);
    }
    article.append(role, bubble);
    ui.messages.append(article);
  }
  requestAnimationFrame(() => {
    ui.messages.scrollTop = follow ? ui.messages.scrollHeight : scrollTop;
  });
}

function renderPrompt(prompt) {
  ui.prompt.replaceChildren();
  ui.prompt.classList.toggle("hidden", !prompt);
  if (!prompt) return;
  const question = document.createElement("p");
  question.textContent = prompt.text;
  const options = document.createElement("div");
  options.className = "prompt-options";
  const choices = prompt.binary ? ["允许", "拒绝"] : prompt.options || [];
  choices.forEach((label, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = `${index + 1}. ${label}`;
    button.disabled = writeBusy;
    button.addEventListener("click", () => choosePrompt(prompt, index));
    options.append(button);
  });
  if (!choices.length) {
    const note = document.createElement("p");
    note.className = "muted";
    note.textContent = "这类交互目前需要在电脑端处理。";
    options.append(note);
  }
  ui.prompt.append(question, options);
}

function applyConversation(result) {
  if (!selected) return;
  if (result.identity) selectedRead = result.identity;
  selectedRevision = result.revision || selectedRevision;
  canSend = Boolean(result.can_send);
  ui.sendSubmit.disabled = writeBusy || !canSend;
  ui.sendText.disabled = !canSend;
  ui.sendText.placeholder = canSend ? "继续输入任务…" : "等待当前回合或问答结束后再发送";
  if (result.unchanged !== true) renderMessages(result.messages || []);
  renderPrompt(result.prompt);
  const state = result.state || selected.state;
  selected = { ...selected, state };
  updateControls();
  ui.conversationMeta.textContent = `${projectName(snapshot, selected.projectId)} · ${agentLabel(selected.kind)} · ${stateLabel(state)}`;
  ui.syncNote.textContent = result.transcriptUnavailable
    ? "会话状态已同步，原生消息记录暂不可读。"
    : `最后同步 ${new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}`;
  ui.syncNote.classList.remove("error");
}

async function readConversation({ silent = false } = {}) {
  clearTimeout(readTimer);
  if (!selected) return;
  const selectionSequence = conversationSequence;
  const requestSequence = ++readSequence;
  const windowId = selected.windowId;
  const paneId = selected.paneId;
  const identity = selectedRead || {
    kind: selected.kind,
    session_id: selected.sessionId,
    epoch: null,
  };
  try {
    const result = await api("/api/v1/session/read", {
      method: "POST",
      body: JSON.stringify({
        windowId,
        paneId,
        identity,
        revision: selectedRevision,
      }),
      timeout: 25_000,
    });
    if (
      requestSequence !== readSequence
      || selectionSequence !== conversationSequence
      || selected?.windowId !== windowId
      || selected?.paneId !== paneId
    ) {
      return;
    }
    applyConversation(result);
  } catch (error) {
    if (
      requestSequence !== readSequence
      || selectionSequence !== conversationSequence
      || selected?.windowId !== windowId
      || selected?.paneId !== paneId
    ) {
      return;
    }
    if (!silent) showToast(errorText(error), true);
    ui.syncNote.textContent = errorText(error);
    ui.syncNote.classList.add("error");
    if (error.code === "conversation_identity_changed" || error.code === "conversation_ended") {
      canSend = false;
      ui.sendSubmit.disabled = true;
      ui.sendText.disabled = true;
    }
  } finally {
    if (
      requestSequence === readSequence
      && selectionSequence === conversationSequence
      && selected?.windowId === windowId
      && selected?.paneId === paneId
    ) {
      readTimer = setTimeout(() => readConversation({ silent: true }), 2600);
    }
  }
}

async function openConversation(session) {
  disarmStop();
  conversationSequence += 1;
  readSequence += 1;
  selected = session;
  selectedRead = null;
  selectedRevision = null;
  canSend = false;
  ui.conversation.classList.remove("hidden");
  ui.conversationTitle.textContent = session.title || agentLabel(session.kind);
  ui.conversationMeta.textContent = `${projectName(snapshot, session.projectId)} · ${agentLabel(session.kind)}`;
  ui.messages.replaceChildren();
  renderPrompt(null);
  updateControls();
  renderSessions();
  await readConversation();
}

function closeConversation(clearSelection = false) {
  disarmStop();
  clearTimeout(readTimer);
  conversationSequence += 1;
  readSequence += 1;
  ui.conversation.classList.add("hidden");
  if (clearSelection) {
    selected = null;
    selectedRead = null;
    selectedRevision = null;
  }
  updateControls();
  renderSessions();
}

async function choosePrompt(prompt, option) {
  if (!selected || writeBusy) return;
  const selectionSequence = conversationSequence;
  try {
    const result = await performWrite(
      "/api/v1/session/choose",
      {
        windowId: selected.windowId,
        paneId: selected.paneId,
        identity: selectedRead || { kind: selected.kind, session_id: selected.sessionId, epoch: null },
        promptId: prompt.id,
        option,
      },
      "choose",
    );
    if (selectionSequence !== conversationSequence) return;
    if (result?.status === "unknown") {
      showToast("电脑没有返回确定回执。请刷新页面核对，旧选项不会自动重放。", true);
    } else {
      showToast("选择已提交，正在等待原生界面更新。");
    }
    renderPrompt(null);
    setTimeout(() => readConversation({ silent: true }), 300);
  } catch (error) {
    if (selectionSequence === conversationSequence) showToast(errorText(error), true);
  }
}

async function sendMessage(event) {
  event.preventDefault();
  const text = ui.sendText.value.trim();
  if (!selected || !text || writeBusy || !canSend) return;
  const selectionSequence = conversationSequence;
  const draft = ui.sendText.value;
  try {
    const result = await performWrite(
      "/api/v1/session/send",
      {
        windowId: selected.windowId,
        paneId: selected.paneId,
        identity: selectedRead,
        text,
      },
      "send",
    );
    if (selectionSequence !== conversationSequence) return;
    if (ui.sendText.value === draft) ui.sendText.value = "";
    ui.sendText.style.height = "";
    canSend = false;
    updateControls();
    if (result?.status === "unknown") {
      showToast("消息可能已经投递，但回执未知。正在刷新会话，不会自动重发。", true);
    } else {
      showToast("消息已交给电脑上的原生会话。");
    }
    setTimeout(() => readConversation({ silent: true }), 500);
  } catch (error) {
    if (selectionSequence === conversationSequence) showToast(errorText(error), true);
  }
}

async function createSession(event) {
  event.preventDefault();
  const projectId = ui.projectSelect.value;
  const prompt = ui.newPrompt.value.trim();
  const kind = document.querySelector('input[name="agent-kind"]:checked')?.value || "codex";
  if (!projectId || !prompt || writeBusy) return;
  const selectionSequence = conversationSequence;
  try {
    const receipt = await performWrite(
      "/api/v1/session/new",
      { projectId, kind, prompt },
      "new",
    );
    if (receipt?.status === "accepted" || receipt?.status === "launching") {
      if (ui.newPrompt.value.trim() === prompt) ui.newPrompt.value = "";
      showToast(receipt.status === "launching" ? "新会话正在准备；出现问答时可先处理选项。" : "新会话已启动，正在刷新活动会话。");
      const next = await refreshState();
      const paneId = receipt.result?.paneId;
      const windowId = receipt.result?.windowId;
      const created = next?.sessions.find(
        (item) => item.paneId === paneId && (windowId == null || item.windowId === windowId),
      );
      if (created && selectionSequence === conversationSequence) await openConversation(created);
    } else if (receipt?.status === "unknown") {
      showToast("启动请求可能已被电脑接受，但未能确认。请先刷新核对，页面不会自动重发。", true);
    } else {
      showToast(`启动没有完成（${receipt?.status || "unknown"}），请查看电脑端状态。`, true);
    }
  } catch (error) {
    showToast(errorText(error), true);
  }
}

async function stopTask() {
  if (!selected || writeBusy) return;
  if (!stopArmed) {
    stopArmed = true;
    ui.stop.textContent = "再次点击停止";
    stopTimer = setTimeout(disarmStop, 3200);
    return;
  }
  disarmStop();
  const selectionSequence = conversationSequence;
  try {
    const result = await performWrite(
      "/api/v1/session/stop",
      {
        windowId: selected.windowId,
        paneId: selected.paneId,
        identity: selectedRead,
      },
      "stop",
    );
    if (selectionSequence !== conversationSequence) return;
    if (result?.status === "unknown") {
      showToast("停止请求的回执未知。请先在电脑端核对，页面不会自动重放。", true);
    } else if (result?.result?.stop?.confirmed) {
      showToast("Hook 已确认当前会话回到可继续状态。");
    } else if (result?.result?.stop) {
      const reason = result.result.stop.reason || "unconfirmed";
      showToast(`停止请求已发送，但执行器没有给出停止确认（${reason}）。`);
    } else {
      showToast("已请求停止；只有电脑上的执行器确认后才表示真正结束。");
    }
    setTimeout(() => readConversation({ silent: true }), 800);
  } catch (error) {
    if (selectionSequence === conversationSequence) showToast(errorText(error), true);
  }
}

function disarmStop() {
  clearTimeout(stopTimer);
  stopArmed = false;
  ui.stop.textContent = "停止";
}

async function recoverPending() {
  const pending = readPending();
  if (!pending.length) return;
  ui.syncNote.textContent = "正在核对上次未确认的操作";
  const results = await Promise.all(pending.map(async (entry) => {
    try {
      const receipt = await pollReceipt(api, entry.requestId, { timeout: 12_000 });
      return { entry, receipt };
    } catch {
      return { entry, receipt: { status: "unknown" } };
    }
  }));
  let accepted = 0;
  let rejected = 0;
  let unknown = 0;
  for (const { entry, receipt } of results) {
    if (receipt.status === "accepted") {
      accepted += 1;
      forgetPending(entry.requestId);
    } else if (receipt.status === "rejected" || receipt.status === "failed") {
      rejected += 1;
      forgetPending(entry.requestId);
    } else {
      unknown += 1;
    }
  }
  if (accepted) {
    showToast(`${accepted} 项上次操作已由电脑接受。`);
    await refreshState({ silent: true }).catch(() => {});
  }
  if (rejected) {
    showToast(`${rejected} 项上次操作未生效。`, true);
  }
  if (unknown) {
    ui.syncNote.textContent = `${unknown} 项操作回执待核对；任务可能仍在运行，不会自动重发。`;
  }
}

function bindEvents() {
  ui.reconnect.addEventListener("click", () => refreshState().catch(() => {}));
  ui.refresh.addEventListener("click", () => refreshState().catch(() => {}));
  const showNew = (visible) => {
    ui.composer.classList.toggle("hidden", !visible);
    ui.newToggle.setAttribute("aria-expanded", String(visible));
    if (visible) ui.newPrompt.focus();
  };
  ui.focusNew.addEventListener("click", () => showNew(true));
  ui.newToggle.addEventListener("click", () => showNew(ui.composer.classList.contains("hidden")));
  ui.collapseNew.addEventListener("click", () => showNew(false));
  ui.newForm.addEventListener("submit", createSession);
  ui.sendForm.addEventListener("submit", sendMessage);
  ui.stop.addEventListener("click", stopTask);
  ui.closeConversation.addEventListener("click", () => closeConversation(false));
  ui.sendText.addEventListener("input", () => {
    ui.sendText.style.height = "auto";
    ui.sendText.style.height = `${Math.min(ui.sendText.scrollHeight, innerHeight * 0.32)}px`;
  });
  ui.sendText.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      ui.sendForm.requestSubmit();
    }
  });
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) refreshState({ silent: true }).catch(() => {});
  });
  addEventListener("online", () => refreshState({ silent: true }).catch(() => {}));
}

async function boot() {
  bindEvents();
  setConnection(ui, false, "连接中");
  try {
    await refreshState();
  } catch {
    showToast("无法连接电脑。请确认 Pebrel 和 Relay 在线后重试。", true);
  }
}

boot();
