const nodes = {
  station: "n-station",
  proxy: "n-proxy",
  quasar: "n-quasar",
  asr: "n-asr",
  processor: "n-processor",
  llm: "n-llm",
  mcp: "n-mcp",
  searxng: "n-searxng",
  tts: "n-tts",
};

const edges = {
  station: "e9",
  proxy: "e1",
  quasar: "e2",
  asr: "e3",
  processor: "e4",
  llm: "e5",
  mcp: "e6",
  searxng: "e7",
  tts: "e8",
};

const logClasses = {
  asr: "asr",
  llm: "llm",
  mcp: "mcp",
  searxng: "mcp",
  tts: "tts",
  quasar: "q",
  processor: "p",
  dashboard: "q",
};

const questionEl = document.getElementById("question");
const answerEl = document.getElementById("answer");
const logsEl = document.getElementById("logs");
const tokenEl = document.getElementById("token-text");
const turnPill = document.getElementById("turn-pill");
const turnId = document.getElementById("turn-id");
const modePill = document.getElementById("mode-pill");
let tokenTimer = null;
let activeTurn = null;

function shortTurn(turn) {
  return "#" + String(turn || "").replace("turn-", "");
}

function formatTime(ts) {
  const date = new Date(ts);
  return date.toLocaleTimeString("ru-RU", { hour12: false }) + "." +
    String(date.getMilliseconds()).padStart(3, "0");
}

function resetTurn(turn) {
  activeTurn = turn;
  turnPill.textContent = shortTurn(turn);
  turnId.textContent = shortTurn(turn) + " · now";
  logsEl.innerHTML = "";
  questionEl.textContent = "…";
  answerEl.textContent = "…";
  tokenEl.textContent = "анализ запроса…";
  clearInterval(tokenTimer);
  document.querySelectorAll(".stage").forEach((stage) => {
    stage.classList.remove("done");
    stage.querySelector(".track span").style.width = "0";
  });
  Object.values(nodes).forEach((id) => {
    document.getElementById(id).classList.remove("active");
  });
  Object.values(edges).forEach((id) => {
    document.getElementById(id).classList.remove("hot");
  });
  Object.keys(nodes).forEach((service) => {
    const metric = document.getElementById("m-" + service);
    if (metric) metric.textContent = service === "station" ? "idle" : "—";
  });
}

function activate(service) {
  const node = document.getElementById(nodes[service]);
  if (node) {
    node.classList.add("active");
    setTimeout(() => node.classList.remove("active"), 850);
  }
  const edge = document.getElementById(edges[service]);
  if (edge) {
    edge.classList.add("hot");
    setTimeout(() => edge.classList.remove("hot"), 700);
  }
}

function mark(stage) {
  const node = document.querySelector('[data-stage="' + stage + '"]');
  if (!node) return;
  node.classList.add("done");
  node.querySelector(".track span").style.width = "100%";
}

function typeTokens(text) {
  clearInterval(tokenTimer);
  tokenEl.textContent = "";
  const parts = text.split(/(\s+)/);
  let index = 0;
  tokenTimer = setInterval(() => {
    tokenEl.textContent += parts[index++] || "";
    if (index >= parts.length) clearInterval(tokenTimer);
  }, 48);
}

function detail(event) {
  const data = event.data || {};
  switch (event.stage) {
    case "turn.start": return "voice session start";
    case "proxy.tls": return "TLS + SNI → quasar";
    case "quasar.session": return "uniproxy /uni.ws";
    case "asr.partial": return "partial: «" + (data.text || "") + "»";
    case "asr.final": return "final: «" + (data.text || "") + "»";
    case "processor.request": return "processor /process";
    case "llm.ttft": return "TTFT " + (data.ttft_ms || event.duration_ms || 0) + " ms";
    case "llm.done":
      return data.pass === 1 ? "first pass · function call" : "answer ready";
    case "mcp.request": return (data.tool || "tool") + "(\"" + (data.query || "") + "\")";
    case "search.results": return (data.count || 0) + " results · " + (data.engine || "");
    case "mcp.response": return (data.chars || 0) + " chars";
    case "tts.chunk":
      return "chunk " + (data.index || 1) + "/" + (data.total || 1) +
        " · " + (data.bytes || 0) + " B";
    case "station.first_audio": return "first audio " + (data.first_audio_ms || 0) + " ms";
    case "station.play": return "TtsSpeak + directives";
    case "metrics": return "vram " + (data.vram_gib || "?") + " GiB · queue " + (data.queue || 0);
    default: return event.stage;
  }
}

function addLog(event) {
  const row = document.createElement("div");
  row.className = "log";
  const serviceClass = logClasses[event.service] || "";
  row.innerHTML = '<span class="t">' + formatTime(event.ts_ms) + "</span>" +
    '<span class="s ' + serviceClass + '">' + event.service + "</span>" +
    "<span>" + detail(event) + "</span>";
  logsEl.prepend(row);
  while (logsEl.children.length > 9) logsEl.lastChild.remove();
}

function applyMetrics(metrics) {
  if (!metrics) return;
  if (metrics.first_audio_ms) {
    document.getElementById("m-first").innerHTML =
      (metrics.first_audio_ms / 1000).toFixed(2) + " <small>s p50</small>";
  }
  if (metrics.ttft_ms) {
    document.getElementById("m-ttft").innerHTML =
      (metrics.ttft_ms / 1000).toFixed(2) + " <small>s</small>";
  }
  if (metrics.vram_gib) {
    document.getElementById("m-vram").innerHTML =
      metrics.vram_gib.toFixed(1) + " <small>/ 8.0 GB</small>";
  }
  if (metrics.queue !== undefined) {
    document.getElementById("m-queue").innerHTML =
      metrics.queue + " <small>tasks</small>";
  }
}

function handle(event) {
  if (!event || !event.stage) return;
  if (event.stage === "turn.start") resetTurn(event.turn_id);
  if (!activeTurn) resetTurn(event.turn_id);
  if (event.turn_id && event.turn_id !== activeTurn && event.stage !== "turn.start") {
    resetTurn(event.turn_id);
  }

  activate(event.service);
  addLog(event);

  const metric = document.getElementById("m-" + event.service);
  if (metric && event.duration_ms) metric.textContent = event.duration_ms + " ms";

  const data = event.data || {};
  switch (event.stage) {
    case "asr.final":
      questionEl.textContent = data.text || "";
      mark("asr");
      break;
    case "llm.ttft":
      mark("ttft");
      applyMetrics({ ttft_ms: data.ttft_ms || event.duration_ms });
      break;
    case "llm.done":
      if (data.pass === 2 || data.pass === undefined) {
        mark("llm2");
        if (data.answer) {
          answerEl.textContent = data.answer;
          typeTokens(data.answer);
        }
      } else {
        mark("llm");
      }
      break;
    case "mcp.response":
      mark("mcp");
      break;
    case "tts.chunk":
      mark("tts");
      break;
    case "station.first_audio":
      applyMetrics({ first_audio_ms: data.first_audio_ms || event.duration_ms });
      break;
    case "metrics":
      applyMetrics(data);
      break;
    default:
      break;
  }
}

function connect() {
  const source = new EventSource("/api/events");
  source.addEventListener("trace", (message) => {
    try {
      handle(JSON.parse(message.data));
    } catch (error) {
      console.error("bad trace event", error);
    }
  });
  source.addEventListener("open", () => {
    modePill.textContent = "LIVE";
  });
  source.addEventListener("error", () => {
    modePill.textContent = "RECONNECT";
  });
}

async function loadState() {
  try {
    const response = await fetch("/api/state");
    const state = await response.json();
    applyMetrics(state.metrics);
    const events = state.events || [];
    if (events.length) {
      const lastTurn = events[events.length - 1].turn_id;
      events.filter((event) => event.turn_id === lastTurn).forEach(handle);
    }
  } catch (error) {
    console.warn("state unavailable", error);
  }
}

const wave = document.getElementById("wave");
for (let index = 0; index < 52; index++) {
  const bar = document.createElement("i");
  bar.style.height = (6 + Math.random() * 24) + "px";
  bar.style.animationDelay = (index * 23 % 700) + "ms";
  wave.appendChild(bar);
}

loadState();
connect();
