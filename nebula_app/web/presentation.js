"use strict";

const runtimeStates = {
  idle: "空闲",
  finished: "已完成",
  running: "运行中",
  thinking: "思考中",
  attention: "等待处理",
  blocked: "等待输入",
  unknown: "状态未知",
};

export function stateLabel(value) {
  return runtimeStates[String(value || "unknown").toLowerCase()] || String(value || "状态未知");
}

export function projectName(snapshot, id) {
  return snapshot?.projects?.find((project) => project.id === id)?.name || "授权项目";
}

export function agentLabel(kind) {
  return kind === "claude" ? "Claude" : "Codex";
}

export function setConnection(ui, online, label) {
  ui.connection.classList.toggle("online", online);
  ui.connection.classList.toggle("offline", !online);
  ui.connection.lastElementChild.textContent = label;
  ui.offline.classList.toggle("hidden", online);
}

export function renderProjects(ui, projects, writeBusy) {
  const previous = ui.projectSelect.value;
  ui.projectSelect.replaceChildren();
  ui.projectList.replaceChildren();
  if (!projects.length) {
    const option = new Option("电脑端尚未授权项目", "");
    option.disabled = true;
    option.selected = true;
    ui.projectSelect.add(option);
    ui.newSubmit.disabled = true;
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "请在电脑的 Pebrel 设置中添加项目，手机不能临时扩大授权。";
    ui.projectList.append(empty);
    return;
  }
  ui.newSubmit.disabled = writeBusy;
  for (const project of projects) {
    ui.projectSelect.add(new Option(project.name, project.id));
    const row = document.createElement("div");
    row.className = "project-item";
    const mark = document.createElement("span");
    mark.className = "agent-mark";
    mark.textContent = project.name.slice(0, 2).toUpperCase();
    const copy = document.createElement("div");
    copy.className = "item-copy";
    const name = document.createElement("strong");
    name.textContent = project.name;
    const path = document.createElement("span");
    path.textContent = project.path;
    path.title = project.path;
    copy.append(name, path);
    row.append(mark, copy);
    ui.projectList.append(row);
  }
  if (projects.some((project) => project.id === previous)) {
    ui.projectSelect.value = previous;
  }
}
