const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const selected = new Set();
let rows = [];
let busy = false;
let lastSnapshot = null;
let toastTimer;

const body = document.querySelector('#service-rows');
const statusNode = document.querySelector('#status');
const dialog = document.querySelector('#detail-dialog');
const escapeHtml = (value) => String(value ?? '').replace(/[&<>"']/g, (char) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));

function notify(message) {
  const toast = document.querySelector('#toast');
  toast.textContent = message;
  toast.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove('show'), 2800);
}

function getActionButtons(row, index) {
  return [
    ['详情', 'detail', true, ''], ['启动', 'start', row.can_start, 'button-success'],
    ['停止', 'stop', row.can_stop, 'button-danger'], ['重启', 'restart', row.can_restart, 'button-primary'],
    ['更新', 'update', row.can_update, ''], ['日志', 'log', true, '']
  ].map(([label, action, allowed, style]) => `<button class="mini-button ${style}" data-action="${action}" data-index="${index}" ${!allowed || (busy && action !== 'detail') ? 'disabled' : ''}>${label}</button>`).join('');
}

function render(snapshot) {
  lastSnapshot = snapshot;
  rows = snapshot.rows;
  busy = snapshot.busy;
  document.querySelector('#directory').textContent = snapshot.directory;
  body.innerHTML = rows.length ? rows.map((row, index) => {
    const isSelected = selected.has(index);
    const state = row.uncertain || row.problem ? '异常' : row.running ? '运行中' : '已停止';
    const stateClass = row.uncertain || row.problem ? 'status-error' : row.running ? 'status-running' : 'status-stopped';
    return `<tr data-row="${index}" class="${isSelected ? 'selected ' : ''}${row.problem || row.uncertain ? 'has-problem' : ''}" title="${escapeHtml(row.note)}">
      <td class="check-col"><input type="checkbox" data-select="${index}" aria-label="选择 ${escapeHtml(row.name)}" ${isSelected ? 'checked' : ''} ${busy ? 'disabled' : ''}></td>
      <td class="name-col"><div class="name-content">${escapeHtml(row.name)}</div></td>
      <td><span class="status-pill ${stateClass}">${state}</span></td>
      <td>${escapeHtml(row.pid)}</td><td class="memory">${escapeHtml(row.memory)}</td>
      <td title="${escapeHtml(row.ports_full)}">${escapeHtml(row.ports)}</td><td>${escapeHtml(row.started)}</td><td>${escapeHtml(row.updated)}</td>
      <td class="actions-cell"><div class="row-actions">${getActionButtons(row,index)}</div></td>
    </tr>`;
  }).join('') : '<tr><td class="empty" colspan="9">配置中暂无应用</td></tr>';

  const all = rows.length > 0 && selected.size === rows.length;
  const selectAll = document.querySelector('#select-all');
  selectAll.checked = all;
  selectAll.indeterminate = selected.size > 0 && !all;
  selectAll.disabled = busy || rows.length === 0;
  document.querySelector('#selection-count').textContent = selected.size ? `已选择 ${selected.size} 个应用` : '未选择应用';
  document.querySelectorAll('[data-bulk]').forEach(button => {
    const action = button.dataset.bulk;
    const available = selected.size > 0 && selectedIndices().some(index => rows[index]?.[`can_${action}`] || (action === 'stop' && rows[index]?.can_stop));
    button.disabled = busy || !available;
  });
  const live = document.querySelector('.live-status');
  live.classList.toggle('busy', busy);
  statusNode.textContent = snapshot.status || '监控中';
  document.querySelectorAll('.mini-button').forEach(button => { if (busy) button.disabled = true; });
  if (dialog.open) {
    const index = Number(dialog.dataset.index);
    if (rows[index]) showDetails(index, false);
  }
}

function selectedIndices() { return [...selected].filter(index => index >= 0 && index < rows.length).sort((a,b) => a-b); }

async function refresh() {
  try { render(await invoke('get_snapshot')); }
  catch (error) { statusNode.textContent = `连接失败：${error}`; }
}

async function runAction(indices, action) {
  if (busy) return;
  if (!indices.length) { notify('请先选择应用'); return; }
  try {
    await invoke('perform_action', { indices, action });
    busy = true;
    statusNode.textContent = `正在执行 ${action}`;
    render({...lastSnapshot, busy: true, status: statusNode.textContent});
  } catch (error) { notify(String(error)); }
}

function showDetails(index, open = true) {
  const row = rows[index];
  if (!row) return;
  dialog.dataset.index = index;
  const fields = [
    ['应用名称', row.name], ['运行状态', row.uncertain || row.problem ? `异常：${row.note}` : row.running ? '运行中' : '已停止'],
    ['PID', row.pid], ['内存', row.memory], ['监听端口', row.ports_full],
    ['启动命令', row.command_line || '—'], ['启动时间', row.started], ['更新时间', row.updated]
  ];
  document.querySelector('#detail-content').innerHTML = fields.map(([label,value]) => `<div class="detail-row"><div class="detail-label">${escapeHtml(label)}</div><div class="detail-value">${escapeHtml(value)}</div></div>`).join('');
  if (open && !dialog.open) dialog.showModal();
}

body.addEventListener('change', event => {
  const checkbox = event.target.closest('[data-select]');
  if (!checkbox) return;
  const index = Number(checkbox.dataset.select);
  checkbox.checked ? selected.add(index) : selected.delete(index);
  if (lastSnapshot) render(lastSnapshot);
});

body.addEventListener('click', event => {
  const actionButton = event.target.closest('[data-action]');
  if (actionButton) {
    event.stopPropagation();
    if (actionButton.dataset.action === 'detail') {
      showDetails(Number(actionButton.dataset.index));
      return;
    }
    runAction([Number(actionButton.dataset.index)], actionButton.dataset.action);
    return;
  }
  if (event.target.closest('.actions-cell') || event.target.closest('.check-col')) return;
  const row = event.target.closest('[data-row]');
  if (!row || busy) return;
  const index = Number(row.dataset.row);
  selected.has(index) ? selected.delete(index) : selected.add(index);
  if (lastSnapshot) render(lastSnapshot);
});

document.querySelector('#select-all').addEventListener('change', event => {
  selected.clear();
  if (event.target.checked) rows.forEach((_, index) => selected.add(index));
  if (lastSnapshot) render(lastSnapshot);
});
document.querySelectorAll('[data-bulk]').forEach(button => button.addEventListener('click', () => runAction(selectedIndices(), button.dataset.bulk)));
document.querySelector('#refresh').addEventListener('click', refresh);
document.querySelector('#close-detail').addEventListener('click', () => dialog.close());
dialog.addEventListener('click', event => { if (event.target === dialog) dialog.close(); });
listen('jkeeper-updated', refresh);
refresh();
setInterval(refresh, 3000);
