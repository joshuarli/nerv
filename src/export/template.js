(function () {
'use strict';

// Load and decode the base64-encoded session JSON.
const b64 = window.__SESSION_B64__ || '';
const jsonStr = new TextDecoder().decode(Uint8Array.from(atob(b64), function(c) { return c.charCodeAt(0); }));
const session = JSON.parse(jsonStr);
const { header, entries } = session;

// Global lookup maps
const entryById = new Map(entries.map(function(e) { return [e.id, e]; }));

// label map: id → label string
const labelMap = new Map();
for (var i = 0; i < entries.length; i++) {
  var e = entries[i];
  if (e.type === 'label') labelMap.set(e.id, e.label);
}

// toolCall prepass: tool_call_id → { name, input/arguments }
// toolResult prepass: tool_call_id → toolResult entry
const toolCallMap = new Map();
const toolResultMap = new Map();
for (var i = 0; i < entries.length; i++) {
  var e = entries[i];
  if (e.type === 'message' && e.role === 'assistant' && Array.isArray(e.content)) {
    for (var j = 0; j < e.content.length; j++) {
      var block = e.content[j];
      // The Rust ContentBlock serializes ToolCall as type:"toolCall" with "arguments"
      // The plan output normalizes to type:"tool_use" with "input" for JS consumption
      if (block.type === 'tool_use' || block.type === 'toolCall') {
        toolCallMap.set(block.id, { name: block.name, input: block.input || block.arguments || {} });
      }
    }
  }
  if (e.type === 'message' && e.role === 'toolResult') {
    toolResultMap.set(e.tool_call_id, e);
  }
}

// marked.js configuration
marked.use({
  breaks: true,
  gfm: true,
  renderer: {
    code: function(token) {
      var lang = token.lang;
      var highlighted;
      if (lang && hljs.getLanguage(lang)) {
        try {
          highlighted = hljs.highlight(token.text, { language: lang }).value;
        } catch(err) {
          highlighted = escapeHtml(token.text);
        }
      } else {
        highlighted = escapeHtml(token.text);
      }
      return '<pre><code class="hljs">' + highlighted + '</code></pre>';
    }
  }
});

// Utility functions
function escapeHtml(s) {
  return String(s || '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

function truncate(s, n) {
  if (!s) return '';
  s = String(s);
  return s.length > n ? s.slice(0, n) + '\u2026' : s;
}

function renderTimestamp(ts) {
  if (!ts) return '';
  var d = new Date(ts);
  if (isNaN(d.getTime())) return '';
  return '<div class="message-timestamp">' + d.toLocaleString() + '</div>';
}

function renderCopyLink(id) {
  return '<button class="copy-link-btn" onclick="copyLink(\'' + id + '\')">&#x1F517;</button>';
}

function renderUsage(u) {
  if (!u) return '';
  var cost = u.cost_usd ? ' &middot; $' + u.cost_usd.toFixed(4) : '';
  var ctx = (u.context_used && u.context_window)
    ? ' &middot; ctx ' + (u.context_used/1000).toFixed(1) + 'k/' + (u.context_window/1000).toFixed(0) + 'k'
    : '';
  return '<div class="message-usage">\u2191' + (u.input||0) + ' \u2193' + (u.output||0) +
    ' cache_read:' + (u.cache_read||0) + ' cache_write:' + (u.cache_write||0) + ctx + cost + '</div>';
}

// Depth cache
var depthCache = new Map();
function getDepth(e) {
  if (depthCache.has(e.id)) return depthCache.get(e.id);
  var depth = 0;
  var cur = e;
  var seen = new Set();
  for (var i = 0; i < 8; i++) {
    if (!cur.parent_id || seen.has(cur.parent_id)) break;
    seen.add(cur.parent_id);
    var parent = entryById.get(cur.parent_id);
    if (!parent) break;
    depth++;
    cur = parent;
  }
  depthCache.set(e.id, depth);
  return depth;
}

function getTreeItemClass(e) {
  if (e.type === 'compaction') return 'tree-item-compaction';
  if (e.type === 'btw') return 'tree-item-btw';
  if (e.type === 'message') {
    switch (e.role) {
      case 'user': return 'tree-item-user';
      case 'assistant': return 'tree-item-assistant';
      case 'toolResult': return 'tree-item-tool' + (e.is_error ? ' error' : '');
      case 'bashExecution': return 'tree-item-tool';
      case 'custom': return e.custom_type === 'btw' ? 'tree-item-btw' : 'tree-item-assistant';
    }
  }
  return '';
}

function getTreeLabel(e) {
  var lbl = labelMap.has(e.id) ? '[' + labelMap.get(e.id) + '] ' : '';
  if (e.type === 'message') {
    switch (e.role) {
      case 'user':
        return lbl + truncate(typeof e.content === 'string' ? e.content : '(user)', 60);
      case 'assistant': {
        var texts = Array.isArray(e.content) ? e.content.filter(function(b) { return b.type === 'text'; }) : [];
        return lbl + 'Assistant: ' + truncate(texts.length ? texts[0].text : '(tools)', 50);
      }
      case 'toolResult': {
        var name = e.tool_name || (toolCallMap.get(e.tool_call_id) || {}).name || 'tool';
        return lbl + (e.is_error ? '\u2717 ' : '\u2713 ') + name;
      }
      case 'bashExecution': return lbl + '$ ' + truncate(e.command, 50);
      case 'custom':
        if (e.custom_type === 'btw') return lbl + '\uD83D\uDCAD ' + truncate(e.content, 50);
        return lbl + (e.custom_type || 'custom') + ': ' + truncate(e.content, 40);
      default: return lbl + (e.role || e.type);
    }
  }
  if (e.type === 'compaction') return lbl + '\u27F3 Compaction';
  if (e.type === 'modelChange') return lbl + '\u27F3 \u2192 ' + (e.model_id || '');
  if (e.type === 'thinkingLevelChange') return lbl + '\uD83E\uDDE0 Thinking: ' + (e.level || '');
  if (e.type === 'systemPrompt') return lbl + 'System Prompt';
  if (e.type === 'branchSummary') return lbl + '\u2937 Branch';
  if (e.type === 'sessionInfo') return lbl + '\uD83D\uDCCB ' + truncate(e.name, 40);
  if (e.type === 'btw') return lbl + '\uD83D\uDCAD ' + truncate(e.note, 50);
  return lbl + e.type;
}

function getSearchText(e) {
  var label = getTreeLabel(e);
  var body = '';
  if (e.type === 'message') {
    if (typeof e.content === 'string') body = e.content;
    else if (Array.isArray(e.content)) {
      body = e.content.filter(function(b) { return b.type === 'text'; }).map(function(b) { return b.text; }).join(' ');
    }
    if (e.command) body += ' ' + e.command;
    if (e.output) body += ' ' + e.output;
    if (e.role === 'toolResult') body += ' ' + (e.content || '') + ' ' + (e.display || '');
  } else if (e.type === 'compaction') {
    body = e.summary || '';
  } else if (e.type === 'btw') {
    body = (e.note || '') + ' ' + (e.response || '');
  }
  return (label + ' ' + body).toLowerCase();
}

// Filter modes
var FILTER_MODES = {
  'default': function(e) {
    if (e.type !== 'message') return true;
    return e.role !== 'toolResult';
  },
  'no-tools': function(e) {
    if (e.type !== 'message') return false;
    return e.role === 'user' || e.role === 'assistant';
  },
  'user-only': function(e) { return e.type === 'message' && e.role === 'user'; },
  'labeled': function(e) { return labelMap.has(e.id); },
  'all': function(e) { return true; }
};
var currentFilter = 'default';

var selectedId = null;

function selectEntry(id) {
  if (selectedId) {
    var prev = document.querySelector('.tree-item[data-id="' + selectedId + '"]');
    if (prev) prev.classList.remove('selected');
  }
  selectedId = id;
  var treeItem = document.querySelector('.tree-item[data-id="' + id + '"]');
  if (treeItem) treeItem.classList.add('selected');
  var msgEl = document.getElementById('entry-' + id);
  if (msgEl) {
    msgEl.scrollIntoView({ behavior: 'smooth', block: 'start' });
    msgEl.classList.add('highlighted');
    setTimeout(function() { msgEl.classList.remove('highlighted'); }, 1500);
  }
}

function renderTree() {
  var q = document.getElementById('search-input').value.trim().toLowerCase();
  var filterFn = FILTER_MODES[currentFilter] || FILTER_MODES['default'];
  var treeEl = document.getElementById('tree');
  treeEl.innerHTML = '';
  var frag = document.createDocumentFragment();
  for (var i = 0; i < entries.length; i++) {
    var e = entries[i];
    if (!filterFn(e)) continue;
    if (q && getSearchText(e).indexOf(q) === -1) continue;
    var depth = getDepth(e);
    var div = document.createElement('div');
    div.className = 'tree-item ' + getTreeItemClass(e);
    div.dataset.id = e.id;
    div.style.paddingLeft = (8 + depth * 12) + 'px';
    div.textContent = getTreeLabel(e);
    (function(eid) {
      div.addEventListener('click', function() { selectEntry(eid); });
    })(e.id);
    frag.appendChild(div);
  }
  treeEl.appendChild(frag);
}

// Sidebar resize
var savedWidth = localStorage.getItem('nerv-sidebar-width');
if (savedWidth) document.documentElement.style.setProperty('--sidebar-width', savedWidth + 'px');

document.getElementById('resize-handle').addEventListener('mousedown', function() {
  function onMove(ev) {
    var w = Math.max(160, Math.min(600, ev.clientX));
    document.documentElement.style.setProperty('--sidebar-width', w + 'px');
  }
  function onUp(ev) {
    localStorage.setItem('nerv-sidebar-width', String(Math.max(160, Math.min(600, ev.clientX))));
    document.removeEventListener('mousemove', onMove);
    document.removeEventListener('mouseup', onUp);
  }
  document.addEventListener('mousemove', onMove);
  document.addEventListener('mouseup', onUp);
});

// Mobile menu
document.getElementById('menu-btn').addEventListener('click', function() {
  document.getElementById('sidebar').classList.toggle('open');
});

// Filter buttons
var filterBtns = document.querySelectorAll('.filter-btn');
for (var i = 0; i < filterBtns.length; i++) {
  (function(btn) {
    btn.addEventListener('click', function() {
      filterBtns.forEach(function(b) { b.classList.remove('active'); });
      btn.classList.add('active');
      currentFilter = btn.dataset.filter;
      renderTree();
    });
  })(filterBtns[i]);
}

// Search
document.getElementById('search-input').addEventListener('input', function() {
  renderTree();
});

// Message rendering

var uidCounter = 0;
function uid() { return 'u' + (++uidCounter); }

function renderExpandablePre(text, blockUid) {
  var lines = text.split('\n');
  if (lines.length <= 5) {
    return '<div class="tool-output"><pre>' + escapeHtml(text) + '</pre></div>';
  }
  var preview = lines.slice(0, 5).join('\n');
  var rest = lines.slice(5).join('\n');
  var remaining = lines.length - 5;
  var restId = 'rest-' + blockUid;
  var linkId = 'expand-' + blockUid;
  return '<div class="tool-output">' +
    '<pre class="tool-output-preview">' + escapeHtml(preview) + '</pre>' +
    '<pre class="tool-output-rest" id="' + restId + '">' + escapeHtml(rest) + '</pre>' +
    '<span class="expand-link" id="' + linkId + '" onclick="expandBlock(\'' + restId + '\',\'' + linkId + '\')">Show ' + remaining + ' more lines \u25BC</span>' +
    '</div>';
}

function renderDiffOutput(text) {
  // Color-code unified diff lines
  var lines = text.split('\n');
  var out = '<div class="tool-output"><pre>';
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i];
    if (line.startsWith('--- ') || line.startsWith('+++ ')) {
      out += '<span class="diff-line-header">' + escapeHtml(line) + '</span>';
    } else if (line.startsWith('@@')) {
      out += '<span class="diff-line-hunk">' + escapeHtml(line) + '</span>';
    } else if (line.startsWith('+')) {
      out += '<span class="diff-line-added">' + escapeHtml(line) + '</span>';
    } else if (line.startsWith('-')) {
      out += '<span class="diff-line-removed">' + escapeHtml(line) + '</span>';
    } else {
      out += '<span class="diff-line-ctx">' + escapeHtml(line) + '</span>';
    }
  }
  out += '</pre></div>';
  return out;
}

function renderToolOutput(toolName, result) {
  if (!result) return '';
  var isError = result.is_error;
  // For errors always use plain content
  if (isError) {
    return '<div class="tool-output" style="color:var(--error)"><pre>' + escapeHtml(result.content || '') + '</pre></div>';
  }
  // Tools where display field contains pre-rendered HTML from Rust
  if ((toolName === 'read' || toolName === 'write' || toolName === 'codemap') && result.display) {
    return '<div class="tool-output">' + result.display + '</div>';
  }
  // Edit tool: display contains the diff (either as HTML from Rust, or plain text)
  if (toolName === 'edit') {
    var diffText = result.display || result.content || '';
    // Check if it looks like a plain-text diff (starts with ---) vs already-rendered HTML
    if (diffText && !diffText.includes('<span')) {
      return renderDiffOutput(diffText);
    }
    if (diffText) {
      return '<div class="tool-output">' + diffText + '</div>';
    }
    return '<div class="tool-output"><pre>' + escapeHtml(result.content || '') + '</pre></div>';
  }
  // Everything else: plain pre block with expand mechanic
  var text = result.content || '';
  return renderExpandablePre(text, uid());
}

function renderThinkingBlock(text, blockUid) {
  return '<div class="thinking-block">' +
    '<span class="thinking-toggle" onclick="toggleEl(\'thinking-' + blockUid + '\')">&#x25B6; Thinking (click to expand)</span>' +
    '<div id="thinking-' + blockUid + '" style="display:none"><pre>' + escapeHtml(text) + '</pre></div>' +
    '</div>';
}

function renderToolBlock(call, result) {
  var isError = result ? result.is_error : false;
  var headerClass = !result ? 'pending' : isError ? 'error' : 'success';
  var icon = !result ? '\u2026' : isError ? '\u2717' : '\u2713';
  var argsObj = call.input || call.arguments || {};
  var argsStr = JSON.stringify(argsObj, null, 2);
  var outputHtml = result ? renderToolOutput(call.name, result) : '';
  return '<div class="tool-block">' +
    '<div class="tool-header ' + headerClass + '">' + icon + ' ' + escapeHtml(call.name) + '</div>' +
    '<div class="tool-args">' + escapeHtml(argsStr) + '</div>' +
    outputHtml +
    '</div>';
}

function renderUser(entry) {
  var content = typeof entry.content === 'string' ? entry.content : '';
  return '<div class="message-block user-message" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    renderTimestamp(entry.timestamp) +
    renderCopyLink(entry.id) +
    '<div class="md-content">' + marked.parse(content) + '</div>' +
    '</div>';
}

function renderAssistant(entry) {
  var html = '';
  var blocks = entry.content || [];
  for (var i = 0; i < blocks.length; i++) {
    var block = blocks[i];
    if (block.type === 'text' && block.text) {
      html += '<div class="md-content">' + marked.parse(block.text) + '</div>';
    } else if (block.type === 'thinking') {
      html += renderThinkingBlock(block.thinking, uid());
    } else if (block.type === 'tool_use' || block.type === 'toolCall') {
      var callInfo = { name: block.name, input: block.input || block.arguments || {} };
      var result = toolResultMap.get(block.id);
      html += renderToolBlock(callInfo, result);
    }
  }
  return '<div class="message-block assistant-message" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    renderTimestamp(entry.timestamp) +
    renderCopyLink(entry.id) +
    html +
    renderUsage(entry.usage) +
    '</div>';
}

function renderBashExecution(entry) {
  var exitOk = entry.exit_code === 0 || entry.exit_code === null || entry.exit_code === undefined;
  var exitClass = exitOk ? 'success' : 'error';
  var icon = exitOk ? '\u2713' : '\u2717';
  return '<div class="message-block" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '<div class="tool-block">' +
    '<div class="tool-header ' + exitClass + '">' + icon + ' $ ' + escapeHtml(truncate(entry.command || '', 80)) + '</div>' +
    renderExpandablePre(entry.output || '', uid()) +
    '</div></div>';
}

function renderBtw(entry) {
  return '<div class="message-block btw-block" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    renderCopyLink(entry.id) +
    '<div class="btw-label">\uD83D\uDCAD btw</div>' +
    '<div class="btw-note">' + escapeHtml(entry.note || entry.content || '') + '</div>' +
    '<div class="btw-response md-content">' + marked.parse(entry.response || '') + '</div>' +
    '</div>';
}

function renderCustomFallback(entry) {
  var content = typeof entry.content === 'string' ? entry.content
    : Array.isArray(entry.content) ? entry.content.filter(function(b) { return b.type === 'text'; }).map(function(b) { return b.text; }).join('\n')
    : '';
  return '<div class="message-block" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '<div class="tool-block">' +
    '<div class="tool-header pending">' + escapeHtml(entry.custom_type || 'custom') + '</div>' +
    '<div class="tool-output"><pre>' + escapeHtml(content) + '</pre></div>' +
    '</div></div>';
}

function renderCompaction(entry) {
  var before = ((entry.tokens_before || 0) / 1000).toFixed(1);
  var after = ((entry.tokens_after || 0) / 1000).toFixed(1);
  var cost = entry.cost_usd_before ? ' &middot; $' + entry.cost_usd_before.toFixed(3) + ' pre-compaction' : '';
  return '<div class="message-block compaction-block" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '<strong>\u27F3 Context compacted</strong> &mdash; ' + before + 'k &rarr; ' + after + 'k tokens' + cost +
    '<div class="md-content" style="margin-top:6px">' + marked.parse(entry.summary || '') + '</div>' +
    '</div>';
}

function renderModelChange(entry) {
  return '<div class="message-block model-change" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '\u27F3 Model changed to <strong>' + escapeHtml(entry.model_id || '') + '</strong>' +
    (entry.provider ? ' (' + escapeHtml(entry.provider) + ')' : '') +
    '</div>';
}

function renderThinkingLevelChange(entry) {
  return '<div class="message-block thinking-level-change" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '\uD83E\uDDE0 Thinking level changed to <strong>' + escapeHtml(entry.level || entry.thinking_level || '') + '</strong>' +
    '</div>';
}

function renderSystemPrompt(entry) {
  var blockUid = 'sysprompt-' + entry.id;
  return '<div class="message-block system-prompt-block" id="entry-' + entry.id + '" data-id="' + entry.id + '">' +
    '<div class="system-prompt-header" onclick="toggleEl(\'' + blockUid + '\')">' +
    '\u25B6 System Prompt (' + (entry.token_count || '?') + ' tokens)</div>' +
    '<pre class="system-prompt-body" id="' + blockUid + '">' + escapeHtml(entry.prompt || '') + '</pre>' +
    '</div>';
}

function renderEntry(entry) {
  if (entry.type === 'message') {
    switch (entry.role) {
      case 'user': return renderUser(entry);
      case 'assistant': return renderAssistant(entry);
      case 'toolResult': return ''; // rendered inline by renderAssistant
      case 'bashExecution': return renderBashExecution(entry);
      case 'custom':
        return entry.custom_type === 'btw' ? renderBtw(entry) : renderCustomFallback(entry);
      default: return '';
    }
  }
  switch (entry.type) {
    case 'compaction': return renderCompaction(entry);
    case 'modelChange': return renderModelChange(entry);
    case 'model_change': return renderModelChange(entry);
    case 'thinkingLevelChange': return renderThinkingLevelChange(entry);
    case 'thinking_level_change': return renderThinkingLevelChange(entry);
    case 'systemPrompt': return renderSystemPrompt(entry);
    case 'system_prompt': return renderSystemPrompt(entry);
    case 'btw': return renderBtw(entry);
    case 'sessionInfo': return ''; // reflected in header only
    case 'session_info': return '';
    case 'label': return ''; // pure metadata
    case 'branchSummary': return '';
    case 'branch_summary': return '';
    default: return '';
  }
}

// Session stats
function computeStats() {
  var users = 0, assistants = 0, toolCalls = 0, toolErrors = 0, compactions = 0;
  var models = new Set();
  var totalInput = 0, totalOutput = 0, totalCacheRead = 0, totalCacheWrite = 0;
  var totalCost = 0;
  var peakContext = 0, peakContextWindow = 0;
  var firstTimestamp = null, lastTimestamp = null;

  for (var i = 0; i < entries.length; i++) {
    var e = entries[i];
    if (e.timestamp) {
      if (!firstTimestamp) firstTimestamp = e.timestamp;
      lastTimestamp = e.timestamp;
    }
    if (e.type === 'message') {
      if (e.role === 'user') users++;
      if (e.role === 'assistant') {
        assistants++;
        var u = e.usage;
        if (u) {
          totalInput += u.input || 0;
          totalOutput += u.output || 0;
          totalCacheRead += u.cache_read || 0;
          totalCacheWrite += u.cache_write || 0;
          totalCost += u.cost_usd || 0;
          if ((u.context_used || 0) > peakContext) {
            peakContext = u.context_used;
            peakContextWindow = u.context_window || 0;
          }
        }
        var blocks = e.content || [];
        for (var j = 0; j < blocks.length; j++) {
          if (blocks[j].type === 'tool_use' || blocks[j].type === 'toolCall') toolCalls++;
        }
      }
      if (e.role === 'toolResult' && e.is_error) toolErrors++;
    }
    if (e.type === 'modelChange' || e.type === 'model_change') {
      if (e.model_id) models.add(e.model_id);
    }
    if (e.type === 'compaction') {
      compactions++;
      totalCost += e.cost_usd_before || 0;
    }
  }

  return {
    users: users, assistants: assistants, toolCalls: toolCalls, toolErrors: toolErrors,
    compactions: compactions, models: models,
    totalInput: totalInput, totalOutput: totalOutput,
    totalCacheRead: totalCacheRead, totalCacheWrite: totalCacheWrite,
    totalCost: totalCost, peakContext: peakContext, peakContextWindow: peakContextWindow,
    firstTimestamp: firstTimestamp, lastTimestamp: lastTimestamp
  };
}

function renderSessionHeader() {
  var stats = computeStats();
  var sessionId = header.id || '';
  var shortId = sessionId.slice(0, 8);
  var ts = stats.firstTimestamp ? new Date(stats.firstTimestamp).toLocaleString() : '';
  var titleParts = ['nerv session'];
  if (shortId) titleParts.push(shortId);
  if (ts) titleParts.push(ts);

  var cacheHitRate = (stats.totalInput > 0)
    ? (stats.totalCacheRead / stats.totalInput * 100).toFixed(1) + '%'
    : 'n/a';

  var peakCtx = stats.peakContext
    ? (stats.peakContext / 1000).toFixed(1) + 'k / ' + (stats.peakContextWindow / 1000).toFixed(0) + 'k'
    : 'n/a';

  var costLine = stats.totalCost > 0
    ? '$' + stats.totalCost.toFixed(4) + (stats.compactions > 0 ? ' (includes ' + stats.compactions + ' compaction' + (stats.compactions > 1 ? 's' : '') + ')' : '')
    : 'n/a';

  var modelList = Array.from(stats.models).join(', ') || 'n/a';

  var errStr = stats.toolErrors > 0 ? ' (' + stats.toolErrors + ' error' + (stats.toolErrors > 1 ? 's' : '') + ')' : '';

  var html = '<div class="session-header-title">' + escapeHtml(titleParts.join(' &middot; ')) + '</div>' +
    '<div class="session-header-stats">' +
    '<strong>Messages:</strong> ' + stats.users + ' user &middot; ' + stats.assistants + ' assistant &middot; ' + stats.toolCalls + ' tool calls' + errStr + '<br>' +
    '<strong>Tokens:</strong> &uarr;' + stats.totalInput.toLocaleString() + ' in &nbsp; &darr;' + stats.totalOutput.toLocaleString() + ' out &nbsp; cache_read: ' + stats.totalCacheRead.toLocaleString() + ' &nbsp; cache_write: ' + stats.totalCacheWrite.toLocaleString() + '<br>' +
    '<strong>Cache hit rate:</strong> ' + cacheHitRate + '<br>' +
    '<strong>Peak ctx:</strong> ' + peakCtx + '<br>' +
    '<strong>Cost:</strong> ' + costLine + '<br>' +
    '<strong>Models:</strong> ' + escapeHtml(modelList) +
    '</div>' +
    '<button class="download-btn" onclick="downloadJsonl()">\u2B07 Download JSONL</button>';

  document.getElementById('session-header').innerHTML = html;
}

// JSONL download
function downloadJsonl() {
  var lines = [];
  lines.push(JSON.stringify({
    type: 'session_header',
    version: 4,
    id: header.id,
    timestamp: header.timestamp
  }));
  for (var i = 0; i < entries.length; i++) {
    lines.push(JSON.stringify(entries[i]));
  }
  var blob = new Blob([lines.join('\n') + '\n'], { type: 'application/x-ndjson' });
  var url = URL.createObjectURL(blob);
  var a = document.createElement('a');
  a.href = url;
  a.download = 'nerv-session-' + ((header.id || 'export').slice(0, 8)) + '.jsonl';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// Global helpers (called from inline onclick)
window.expandBlock = function(restId, linkId) {
  var restEl = document.getElementById(restId);
  var linkEl = document.getElementById(linkId);
  if (restEl) restEl.classList.add('expanded');
  if (linkEl) linkEl.style.display = 'none';
};

window.toggleEl = function(id) {
  var el = document.getElementById(id);
  if (!el) return;
  el.classList.toggle('expanded');
  if (el.style.display === 'none') el.style.display = '';
  else el.style.display = 'none';
};

window.copyLink = function(id) {
  var url = location.origin + location.pathname + '#entry-' + id;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(url).catch(function() {});
  }
};

window.downloadJsonl = downloadJsonl;

function toggleAllThinking() {
  var blocks = document.querySelectorAll('.thinking-block > div[id]');
  var anyHidden = false;
  for (var i = 0; i < blocks.length; i++) {
    if (blocks[i].style.display === 'none' || !blocks[i].style.display) { anyHidden = true; break; }
  }
  for (var i = 0; i < blocks.length; i++) {
    blocks[i].style.display = anyHidden ? '' : 'none';
  }
}

function toggleAllToolOutput() {
  var rests = document.querySelectorAll('.tool-output-rest');
  var anyCollapsed = false;
  for (var i = 0; i < rests.length; i++) {
    if (!rests[i].classList.contains('expanded')) { anyCollapsed = true; break; }
  }
  var links = document.querySelectorAll('.expand-link');
  for (var i = 0; i < rests.length; i++) {
    if (anyCollapsed) rests[i].classList.add('expanded');
    else rests[i].classList.remove('expanded');
  }
  for (var i = 0; i < links.length; i++) {
    links[i].style.display = anyCollapsed ? 'none' : '';
  }
}

// Keyboard shortcuts
document.addEventListener('keydown', function(e) {
  var searchEl = document.getElementById('search-input');
  if (e.key === 'Escape' && document.activeElement !== searchEl) {
    searchEl.value = '';
    renderTree();
  }
  if (e.ctrlKey && e.key === 't') { e.preventDefault(); toggleAllThinking(); }
  if (e.ctrlKey && e.key === 'o') { e.preventDefault(); toggleAllToolOutput(); }
});

// URL anchor on load
window.addEventListener('load', function() {
  var hash = location.hash;
  if (hash && hash.startsWith('#entry-')) {
    var el = document.getElementById(hash.slice(1));
    if (el) {
      el.scrollIntoView({ block: 'start' });
      el.classList.add('highlighted');
    }
  }
});

// Initial render — messages
(function initRender() {
  var messagesEl = document.getElementById('messages');
  var frag = document.createDocumentFragment();
  for (var i = 0; i < entries.length; i++) {
    var html = renderEntry(entries[i]);
    if (!html) continue;
    var tpl = document.createElement('template');
    tpl.innerHTML = html;
    frag.appendChild(tpl.content);
  }
  messagesEl.appendChild(frag);
})();

renderSessionHeader();
renderTree();

})();
