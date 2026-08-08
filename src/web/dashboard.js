/* ---------- render ----------
   Compute every value that crosses tab boundaries once into `c`, then render
   only the active tab (hidden tabs re-render on switch, so their charts size
   to a real clientWidth). */
let activeTab = 'overview';
/* Ids spelled out rather than built from the tab name: a concatenated lookup
   is invisible to the untagged-string linter, which is the one thing that
   stops English creeping back. */
const TITLES = {
  overview: 'dashboard.nav.tab.overview', models: 'dashboard.nav.tab.models',
  clients: 'dashboard.nav.tab.clients', reliability: 'dashboard.nav.tab.reliability',
  capacity: 'dashboard.nav.tab.capacity', settings: 'dashboard.nav.tab.settings',
};

function render() {
  if (!samples.length) return;
  const last = samples[samples.length-1], rows = nowRows;
  // Keep this plain. Consumers that place it in fixed markup escape it at
  // their own HTML sink.
  const windowLabel = catalogMessage('dashboard.common.note.selected_time_range');

  /* ----- shared per-model aggregates ----- */
  const ptok = wgroups('nimproxy_prompt_tokens_total', 'model');
  const ctok = wgroups('nimproxy_completion_tokens_total', 'model');
  const reqModels = wgroups('nimproxy_requests_total', 'model', l => l.path === '/v1/chat/completions');
  const models = [...new Set([...ctok.keys(), ...ptok.keys(), ...reqModels.keys()])].filter(m => m !== 'none');
  models.sort((a,b) => (ctok.get(b)||0) - (ctok.get(a)||0));
  let allP = 0, allC = 0;
  for (const m of models) {
    allP += ptok.get(m) || 0; allC += ctok.get(m) || 0;
  }
  const wsum = name => windowed(s => sum(s.rows, name));
  const ttftS = wsum('nimproxy_ttft_seconds_sum'), ttftN = wsum('nimproxy_ttft_seconds_count');
  const tpsS = wsum('nimproxy_tokens_per_second_sum'), tpsN = wsum('nimproxy_tokens_per_second_count');
  const okByModel = wgroups('nimproxy_requests_total', 'model', l => IS_2XX(l.status));
  const errByModel = wgroups('nimproxy_requests_total', 'model', l => IS_ERR(l.status));
  const ttftMS = wgroups('nimproxy_ttft_seconds_sum', 'model'), ttftMC = wgroups('nimproxy_ttft_seconds_count', 'model');
  const tpsMS = wgroups('nimproxy_tokens_per_second_sum', 'model'), tpsMC = wgroups('nimproxy_tokens_per_second_count', 'model');
  const errRate = m => { const e = errByModel.get(m) || 0, all = e + (okByModel.get(m) || 0); return all ? e / all * 100 : NaN; };
  const errPct = m => { const v = errRate(m); return isFinite(v) ? pctOf(v / 100, v ? 1 : 0) : NO_VALUE; };
  // Quality maps shared by the model table, finish-reason panel, and scorecard.
  const tpotMS = wgroups('nimproxy_tpot_seconds_sum', 'model'), tpotMC = wgroups('nimproxy_tpot_seconds_count', 'model');
  const finTotal = wgroups('nimproxy_finish_reason_total', 'model');
  const finLen = wgroups('nimproxy_finish_reason_total', 'model', l => l.reason === 'length');
  const reasoningByModel = wgroups('nimproxy_reasoning_tokens_total', 'model');
  const tpotAvg = m => tpotMC.get(m) ? tpotMS.get(m) / tpotMC.get(m) : NaN;
  const truncPct = m => finTotal.get(m) ? (finLen.get(m) || 0) / finTotal.get(m) * 100 : NaN;
  const reasonPct = m => ctok.get(m) ? (reasoningByModel.get(m) || 0) / ctok.get(m) * 100 : NaN;
  const pct = v => isFinite(v) ? pctOf(v / 100, v && v < 10 ? 1 : 0) : NO_VALUE;

  /* ----- shared proxy / capacity aggregates ----- */
  const reqPts = rateSeries(s => sum(s.rows, 'nimproxy_requests_total'));
  const capacityPoints = (rangeData?.points || []).map(point => {
    const duration = +point.duration_seconds || 0;
    const requestDelta = sum((point.values || []).map(asRow), 'nimproxy_requests_total');
    const rpm = duration > 0 ? requestDelta / duration * 60 : 0;
    const historicalCapacity = point.capacity?.average_rpm;
    return {
      t: +point.to * 1000, duration, rpm,
      capacity: isFinite(historicalCapacity) && historicalCapacity > 0 ? +historicalCapacity : null,
    };
  });
  const curRpm = nowRpm;
  const avgRpm = reqPts.length ? reqPts.reduce((a,p) => a+p.v, 0) / reqPts.length : 0;
  const peakRpm = reqPts.length ? Math.max(...reqPts.map(p => p.v)) : 0;
  const capacity = poolCapacity();
  const rpmNow = curRpm;
  const capRatio = capacity > 0 ? rpmNow / capacity : 0;
  // Green while there's headroom, amber approaching the cap, red once saturated.
  const capColor = capRatio >= 0.9 ? css('--red') : capRatio >= 0.7 ? css('--amber') : MED;
  const wreq = windowed(s => sum(s.rows, 'nimproxy_requests_total'));
  const wok = windowed(s => sum(s.rows, 'nimproxy_requests_total', l => IS_2XX(l.status)));
  const okRatio = wreq ? wok/wreq : 1;
  const okColor = okRatio < 0.9 ? css('--red') : okRatio < 0.99 ? css('--amber') : css('--green');

  /* ----- shared client aggregates ----- */
  const cReq = wgroups('nimproxy_requests_total', 'client');
  const cP = wgroups('nimproxy_prompt_tokens_total', 'client');
  const cC = wgroups('nimproxy_completion_tokens_total', 'client');
  const clients = [...cReq.keys()].sort((a,b) => (cC.get(b)||0) - (cC.get(a)||0));
  const streamT = wgroups('nimproxy_stream_requests_total', 'client', l => l.stream === 'true');
  const streamF = wgroups('nimproxy_stream_requests_total', 'client', l => l.stream === 'false');
  const toolReqs = wgroups('nimproxy_request_tools_count', 'client');
  const totStreamT = [...streamT.values()].reduce((a, b) => a + b, 0);
  const totStreamF = [...streamF.values()].reduce((a, b) => a + b, 0);
  const totGen = totStreamT + totStreamF;
  const totToolReq = [...toolReqs.values()].reduce((a, b) => a + b, 0);

  /* ----- shared reliability / security counters ----- */
  const shed = windowed(s => sum(s.rows, 'nimproxy_shed_total'));
  const unauth = windowed(s => sum(s.rows, 'nimproxy_unauthorized_total'));
  const logins = windowed(s => sum(s.rows, 'nimproxy_login_failures_total'));
  const cooldown429 = windowed(s => sum(s.rows, 'nimproxy_lane_cooldown_total', l => l.status === '429'));
  const cooldownOther = windowed(s => sum(s.rows, 'nimproxy_lane_cooldown_total', l => l.status !== '429'));
  const qwS = wsum('nimproxy_queue_wait_seconds_sum'), qwN = wsum('nimproxy_queue_wait_seconds_count');
  const upS = wsum('nimproxy_upstream_seconds_sum'), upN = wsum('nimproxy_upstream_seconds_count');
  const statusG = wgroups('nimproxy_requests_total', 'status');
  const observationFields = new Set([
    'prompt_tokens', 'completion_tokens', 'total_tokens', 'cached_tokens', 'reasoning_tokens',
  ]);
  const observationResults = ['invalid', 'unavailable', 'estimated', 'measured'];
  const observationCounts = Object.fromEntries(observationResults.map(result => [result, 0]));
  let observationPresent = false;
  for (const row of last.rows) {
    if (row.name !== 'nimproxy_usage_observations_total'
        || !observationFields.has(row.labels.field)
        || !observationResults.includes(row.labels.result)
        || !Number.isFinite(+row.value) || +row.value < 0) continue;
    observationPresent = true;
    observationCounts[row.labels.result] += +row.value;
  }
  const observationMax = Math.max(...Object.values(observationCounts));
  const observationResult = !observationPresent ? 'unavailable'
    : observationMax === 0 ? 'zero'
    : observationResults.find(result => observationCounts[result] === observationMax);
  const observationQuality = {
    invalid: {
      value: catalogMessage('dashboard.overview.observation_quality.invalid'), tone: 'crit',
    },
    unavailable: {
      value: catalogMessage('dashboard.overview.observation_quality.unavailable'), tone: 'warn',
    },
    estimated: {
      value: catalogMessage('dashboard.overview.observation_quality.estimated'), tone: 'warn',
    },
    measured: {
      value: catalogMessage('dashboard.overview.observation_quality.measured'), tone: undefined,
    },
    zero: {
      value: catalogMessage('dashboard.overview.observation_quality.zero'), tone: 'zero',
    },
  }[observationResult];

  const c = {
    last, rows, windowLabel,
    ptok, ctok, reqModels, models, allP, allC,
    wsum, ttftS, ttftN, tpsS, tpsN,
    okByModel, errByModel, ttftMS, ttftMC, tpsMS, tpsMC, errRate, errPct,
    tpotMS, tpotMC, finTotal, finLen, reasoningByModel, tpotAvg, truncPct, reasonPct, pct,
    reqPts, capacityPoints, curRpm, avgRpm, peakRpm, capacity, rpmNow, capRatio, capColor, wreq, wok, okRatio, okColor,
    cReq, cP, cC, clients, streamT, streamF, toolReqs, totStreamT, totStreamF, totGen, totToolReq,
    shed, unauth, logins, cooldown429, cooldownOther, qwS, qwN, upS, upN, statusG, observationQuality,
  };
  (RENDERERS[activeTab] || renderOverview)(c);
}

/* client monogram chip (client leaderboards) */
const clientChip = cl =>
  `<span class="logo" data-style="background:${clientColor(cl)}">${escapeHtml(initialsOf(String(cl)))}</span>`;

/* ===== overview ===== */
function renderOverview(c) {
  const { windowLabel, capRatio, capColor, okRatio, okColor, wreq, wok, allP, allC,
    reqPts, rows, shed, unauth, logins, cooldown429, rpmNow, capacity, curRpm,
    models, ctok, clients, cC, errRate, observationQuality } = c;
  const ctokPts = rateSeries(s => sum(s.rows, 'nimproxy_completion_tokens_total'));
  const okPts = samples.slice(1).map((s, i) => {
    const dr = sum(s.rows, 'nimproxy_requests_total') - sum(samples[i].rows, 'nimproxy_requests_total');
    const dok = sum(s.rows, 'nimproxy_requests_total', l => IS_2XX(l.status)) - sum(samples[i].rows, 'nimproxy_requests_total', l => IS_2XX(l.status));
    return dr > 0 ? { t: s.t, v: Math.max(0, Math.min(100, dok / dr * 100)) } : null;
  }).filter(Boolean);
  kpiCards($('o-kpis'), [
    { icon: 'pulse', label: catalogMessage('dashboard.common.col.requests'), value: fmt(wreq), sub: windowLabel, delta: deltaChip(reqPts), pts: reqPts },
    { icon: 'stack', label: catalogMessage('dashboard.common.col.tokens_out'), value: fmt(allC), sub: catalogMessage('dashboard.overview.kpi.tokens_in', { tokens: fmt(allP) }), delta: deltaChip(ctokPts), pts: ctokPts },
    { icon: 'check', label: catalogMessage('dashboard.common.kpi.success_rate'), value: wreq ? pctOf(okRatio, 2) : NO_VALUE,
      sub: catalogMessage('dashboard.overview.kpi.success_counts', { ok: fmt(wok), requests: fmt(wreq) }), delta: deltaChip(okPts), pts: okPts },
  ], true);

  setMessageText($('o-reqnow'), 'dashboard.common.value.now', { value: fmt(curRpm) });
  lineChart($('o-traffic'), [{ name: catalogMessage('dashboard.chart.requests_per_min'), color: MED, pts: reqPts, area: true }], fmt, { height: 220 });

  $('o-rings').innerHTML = '<div class="ringwrap" id="o-ring-cap"></div><div class="ringwrap" id="o-ring-ok"></div>';
  // ringGauge accepts an id and owns its separate HTML and attribute sinks.
  ringGauge($('o-ring-cap'), capRatio, pctOf(capRatio, 0), capColor,
    'dashboard.overview.ring.capacity_used', capacityKeySummary(rpmNow, capacity, cfg.lanes));
  const errShare = wreq ? (wreq - wok) / wreq * 100 : 0;
  ringGauge($('o-ring-ok'), okRatio, wreq ? pctOf(okRatio, 0) : NO_VALUE, okColor,
    'dashboard.common.kpi.success_rate', errorCooldownSummary(
      errShare, cooldown429));
  $('o-health').innerHTML =
    metricRow(catalogMessage('dashboard.common.row.active_now'), fmt(sum(rows, 'nimproxy_active_requests'))) +
    metricRow(catalogMessage('dashboard.common.row.queued'), fmt(sum(rows, 'nimproxy_queue_depth'))) +
    metricRow(catalogMessage('dashboard.overview.row.observation_quality'), observationQuality.value, observationQuality.tone) +
    metricRow(catalogMessage('dashboard.common.row.rate_limit_cooldowns'), fmt(cooldown429), cooldown429 > 0 ? 'warn' : 'zero') +
    metricRow(catalogMessage('dashboard.common.row.dropped'), fmt(shed), shed > 0 ? 'crit' : 'zero') +
    metricRow(catalogMessage('dashboard.common.row.unauthorized'), fmt(unauth), unauth > 0 ? 'crit' : 'zero') +
    metricRow(catalogMessage('dashboard.common.row.failed_logins'), fmt(logins), logins > 0 ? 'crit' : 'zero');

  const perfBlock = (label, note, p50, p95, fmtV) => {
    const scale = Math.max(isFinite(p50) ? p50 : 0, isFinite(p95) ? p95 : 0, 1e-9) * 1.25;
    const pos = v => Math.min(96, v / scale * 100).toFixed(1) + '%';
    return `<div><div data-style="display:flex;justify-content:space-between;align-items:baseline;margin-bottom:14px">` +
      `<span data-style="font-size:12.5px;color:var(--ink-2)">${escapeHtml(label)}</span><span data-style="font:400 10px var(--mono);color:var(--ink-3)">${escapeHtml(note)}</span></div>` +
      `<div class="ptrack">${isFinite(p50) ? `<div class="p50" data-style="left:${pos(p50)}"></div>` : ''}${isFinite(p95) ? `<div class="p95" data-style="left:${pos(p95)}"></div>` : ''}</div>` +
      `<div class="pvals"><div><div class="l">p50</div><div class="v" data-style="color:var(--green-lt)">${fmtV(p50)}</div></div>` +
      `<div data-style="text-align:right"><div class="l">p95</div><div class="v" data-style="color:var(--ink-2)">${fmtV(p95)}</div></div></div></div>`;
  };
  const q = (metric, f) => { const b = wbuckets(metric, f); return [quantile(b, 0.5), quantile(b, 0.95)]; };
  const [t50, t95] = q('nimproxy_ttft_seconds');
  const [s50, s95] = q('nimproxy_tokens_per_second', l => l.source === 'usage');
  const [i50, i95] = q('nimproxy_tpot_seconds');
  $('o-perf').innerHTML =
    perfBlock(catalogMessage('dashboard.models.ttft.heading'), catalogMessage('dashboard.overview.perf.lower_is_better'), t50, t95, secs) +
    perfBlock(catalogMessage('dashboard.models.genspeed.heading'), catalogMessage('dashboard.overview.perf.higher_is_better'), s50, s95, v => isFinite(v) ? fmt(v) + ' tok/s' : '–') +
    perfBlock(catalogMessage('dashboard.models.itl.heading'), catalogMessage('dashboard.overview.perf.lower_is_better'), i50, i95, secs);

  leaderList($('o-topmodels'), models.slice(0, 5).map(m => {
    const pub = publisher(m);
    return { name: m, label: prettyName(m), chip: chipHtml(pub), color: pub.color, v: ctok.get(m) || 0 };
  }), fmt);
  leaderList($('o-topharness'), clients.slice(0, 5).map(cl =>
    ({ name: cl, chip: clientChip(cl), color: clientColor(cl), v: cC.get(cl) || 0 })), fmt);
}

/* ===== models (includes the head-to-head scorecard, ex-Compare tab) ===== */
function renderModels(c) {
  const { models, ptok, ctok, reqModels, allP, allC, windowLabel,
    ttftS, ttftN, tpsS, tpsN, okByModel, ttftMS, ttftMC, tpsMS, tpsMC, errRate, errPct,
    finTotal, reasoningByModel, tpotAvg, truncPct, reasonPct, pct } = c;

  const chatReqs = [...reqModels.values()].reduce((a, b) => a + b, 0);
  const qmed = (metric, f) => { const v = quantile(wbuckets(metric, f), 0.5); return isFinite(v) ? v : NaN; };
  kpiCards($('m-kpis'), [
    { icon: 'pulse', label: catalogMessage('dashboard.common.col.requests'), value: fmt(chatReqs), sub: modelCount(models.length),
      pts: rateSeries(s => sum(s.rows, 'nimproxy_requests_total', l => l.path === '/v1/chat/completions')) },
    { icon: 'stack', label: catalogMessage('dashboard.models.kpi.completion_tokens'), value: fmt(allC), sub: windowLabel,
      pts: rateSeries(s => sum(s.rows, 'nimproxy_completion_tokens_total')) },
    { icon: 'grid', label: catalogMessage('dashboard.models.kpi.prompt_tokens'), value: fmt(allP), sub: windowLabel,
      pts: rateSeries(s => sum(s.rows, 'nimproxy_prompt_tokens_total')) },
    { icon: 'clock', label: catalogMessage('dashboard.models.col.avg_ttft'), value: ttftN ? secs(ttftS / ttftN) : '–',
      sub: catalogMessage('dashboard.models.stat.median', { value: secs(qmed('nimproxy_ttft_seconds')) }),
      pts: avgSeries('nimproxy_ttft_seconds_sum', 'nimproxy_ttft_seconds_count') },
    { icon: 'bolt', label: catalogMessage('dashboard.models.kpi.avg_speed'), value: tpsN ? fmt(tpsS / tpsN) + ' tok/s' : '–',
      sub: catalogMessage('dashboard.models.stat.median', { value: isFinite(qmed('nimproxy_tokens_per_second', l => l.source === 'usage')) ? fmt(qmed('nimproxy_tokens_per_second', l => l.source === 'usage')) + ' tok/s' : '–' }),
      pts: avgSeries('nimproxy_tokens_per_second_sum', 'nimproxy_tokens_per_second_count') },
  ]);

  const top = models.slice(0, 5), topColors = seriesColors(top);
  const tokSeries = top.map((m, i) => ({
    name: prettyName(m), color: topColors[i],
    pts: rateSeries(s => sum(s.rows, 'nimproxy_completion_tokens_total', l => l.model === m)),
  }));
  lineChart($('chart-modeltok'), tokSeries, fmt, { height: 220 });
  legend($('legend-modeltok'), tokSeries);

  /* quantile quad: trend chart + current windowed p50/p95 beneath */
  const quad = (chartId, valId, metric, f, fmtV) => {
    const series = quantSeries(metric, f);
    lineChart($(chartId), series, fmtV, { height: 120 });
    const b = wbuckets(metric, f);
    $(valId).innerHTML =
      `<span><i data-style="background:${MED}"></i>${escapeHtml(catalogMessage('dashboard.models.stat.median', { value: fmtV(quantile(b, 0.5)) }))}</span>` +
      `<span><i data-style="background:${P95}"></i>${escapeHtml(catalogMessage('dashboard.models.stat.p95', { value: fmtV(quantile(b, 0.95)) }))}</span>`;
  };
  quad('chart-ttft', 'q-ttft', 'nimproxy_ttft_seconds', null, secs);
  quad('chart-tps', 'q-tps', 'nimproxy_tokens_per_second', l => l.source === 'usage', v => isFinite(v) ? fmt(v) + ' tok/s' : '–');
  quad('chart-tpot', 'q-tpot', 'nimproxy_tpot_seconds', null, secs);
  quad('chart-upstream', 'q-upstream', 'nimproxy_upstream_seconds', null, secs);

  /* how responses end */
  // enum key (persisted, never translated) · label · severity class
  const FINISH = [['stop', catalogMessage('dashboard.models.finish.completed'), ''],
    ['length', catalogMessage('dashboard.models.col.truncated'), 'critc'],
    ['tool_calls', catalogMessage('dashboard.models.finish.tool_call'), ''],
    ['content_filter', catalogMessage('dashboard.models.finish.filtered'), 'warnc'],
    ['other', catalogMessage('dashboard.models.finish.other'), '']];
  const finByReason = Object.fromEntries(FINISH.map(([r]) => [r, wgroups('nimproxy_finish_reason_total', 'model', l => l.reason === r)]));
  const finModels = models.filter(m => finTotal.get(m));
  if (finModels.length) setMessageText($('m-fincount'), modelCountId(finModels.length), { n: fmt(finModels.length) });
  else $('m-fincount').replaceChildren();
  sortTable($('table-finish'), 'finish',
    [{ label: catalogMessage('dashboard.common.col.model'), align: 'l', str: true }, ...FINISH.map(([, l]) => ({ label: l }))],
    finModels.map(m => {
      const tot = finTotal.get(m) || 1;
      const shares = FINISH.map(([r]) => (finByReason[r].get(m) || 0) / tot * 100);
      return {
        vals: [prettyName(m), ...shares],
        cells: [`<i class="sw" data-style="background:${publisher(m).color}"></i>${escapeHtml(prettyName(m))}`,
          ...shares.map((v, i) => `<span class="${v >= 0.5 ? FINISH[i][2] : 'dim'}">${pct(v)}</span>`)],
      };
    }), { defaultIdx: 1, maxH: 300, minW: 460, empty: catalogMessage('dashboard.common.empty.no_completed_requests') });

  const reasoningModels = models.filter(m => (reasoningByModel.get(m) || 0) > 0);
  if (!reasoningModels.length) $('meters-reasoning').innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.models.empty.no_reasoning'))}</div>`;
  else barList($('meters-reasoning'),
    reasoningModels.map(m => ({ name: prettyName(m), v: reasonPct(m), color: publisher(m).color })), pct);

  /* tool calls actually emitted per model (calls, not requests offering tools).
     Hidden until a generation emits one. */
  const toolCalls = wgroups('nimproxy_tool_calls_total', 'model');
  const tcModels = models.filter(m => (toolCalls.get(m) || 0) > 0)
    .sort((a, b) => (toolCalls.get(b) || 0) - (toolCalls.get(a) || 0));
  $('card-toolcalls').hidden = !tcModels.length;
  if (tcModels.length) barList($('m-toolcalls'), tcModels.map(m => {
    const n = toolCalls.get(m) || 0, r = reqModels.get(m) || 0;
    return { name: m, label: prettyName(m), v: n,
      vtext: r ? catalogMessage('dashboard.models.stat.tool_calls_per_request', { calls: fmt(n), per_request: fmt(n / r) }) : fmt(n), color: publisher(m).color };
  }), fmt);

  /* scorecard (ex-Compare): best in each column highlighted, still sortable */
  const avgTtft = m => ttftMC.get(m) ? ttftMS.get(m) / ttftMC.get(m) : NaN;
  const avgTps = m => tpsMC.get(m) ? tpsMS.get(m) / tpsMC.get(m) : NaN;
  const avgReply = m => okByModel.get(m) ? (ctok.get(m) || 0) / okByModel.get(m) : NaN;
  const cmp = models.slice(0, 8);
  sortTable($('compare-scorecard'), 'scorecard', [
    { label: catalogMessage('dashboard.common.col.model'), align: 'l', str: true },
    { label: catalogMessage('dashboard.common.col.requests'), best: 'max' }, { label: 'TTFT', best: 'min' }, { label: 'tok/s', best: 'max' },
    { label: 'TPOT', best: 'min' }, { label: catalogMessage('dashboard.models.col.avg_response'), best: 'max' }, { label: catalogMessage('dashboard.models.col.truncated'), best: 'min' },
    { label: catalogMessage('dashboard.models.col.reasoning'), best: 'min' }, { label: catalogMessage('dashboard.models.col.errors'), best: 'min' },
  ], cmp.map(m => ({
    vals: [prettyName(m), reqModels.get(m) || 0, avgTtft(m), avgTps(m), tpotAvg(m), avgReply(m), truncPct(m), reasonPct(m), errRate(m)],
    cells: [`<i class="sw" data-style="background:${publisher(m).color}"></i>${escapeHtml(prettyName(m))}`,
      fmt(reqModels.get(m) || 0), isFinite(avgTtft(m)) ? secs(avgTtft(m)) : '–', isFinite(avgTps(m)) ? fmt(avgTps(m)) : '–',
      isFinite(tpotAvg(m)) ? secs(tpotAvg(m)) : '–', isFinite(avgReply(m)) ? fmt(avgReply(m)) + ' tok' : '–',
      pct(truncPct(m)), pct(reasonPct(m)), pct(errRate(m))],
  })), { defaultIdx: 1, maxH: 360, minW: 720 });
  barList($('compare-bars'), cmp.map(m => ({ name: prettyName(m), v: avgTps(m), color: publisher(m).color })),
    v => fmt(v) + ' tok/s');

  /* leading model cards (top 4; the rest live in the table) */
  if (models.length) setMessageText($('m-cardnote'), 'dashboard.models.cards.note');
  else $('m-cardnote').textContent = '';
  $('modelcards').innerHTML = models.slice(0, 4).map((m, i) => {
    const pub = publisher(m), ct = ctok.get(m) || 0, e = errRate(m);
    const stat = (l, v, cls) => `<div class="stat"><div class="l">${escapeHtml(l)}</div><div class="v${cls ? ' ' + cls : ''}">${escapeHtml(v)}</div></div>`;
    return `<div class="mcard"><span class="rank">#${i + 1}</span>
      <div class="mbrand">${chipHtml(pub)}
        <div><div class="mname" title="${escapeHtml(m)}">${escapeHtml(prettyName(m))}</div><div class="mpub">${escapeHtml(pub.name)}</div></div>
      </div>
      <div class="mstats">
        ${stat(catalogMessage('dashboard.common.col.requests'), fmt(reqModels.get(m) || 0))}
        ${stat(catalogMessage('dashboard.common.note.tokens_out'), fmt(ct))}
        ${stat('TTFT', ttftMC.get(m) ? secs(avgTtft(m)) : '–')}
        ${stat('tok/s', tpsMC.get(m) ? fmt(avgTps(m)) : '–')}
        ${stat(catalogMessage('dashboard.models.col.errors'), errPct(m), isFinite(e) && e > 0 ? 'critc' : 'dim')}
      </div></div>`;
  }).join('') || `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_traffic'))}</div>`;

  /* per-model table */
  const modelCountNode = $('m-tablecount');
  if (models.length) {
    setMessageText(modelCountNode, modelCountWithSortId(models.length), { n: fmt(models.length) });
  } else {
    modelCountNode.replaceChildren();
  }
  sortTable($('table-models'), 'models', [
    { label: catalogMessage('dashboard.common.col.model'), align: 'l', str: true }, { label: catalogMessage('dashboard.common.col.requests') }, { label: catalogMessage('dashboard.common.col.tokens_out') },
    { label: catalogMessage('dashboard.models.col.avg_ttft') }, { label: catalogMessage('dashboard.models.col.avg_tps') }, { label: 'TPOT' }, { label: catalogMessage('dashboard.models.col.avg_response') },
    { label: catalogMessage('dashboard.models.col.truncated') }, { label: catalogMessage('dashboard.models.col.reasoning') }, { label: catalogMessage('dashboard.models.col.errors') },
  ], models.map(m => {
    const ct = ctok.get(m) || 0, ok = okByModel.get(m) || 0, e = errRate(m);
    return {
      vals: [m, reqModels.get(m) || 0, ct, avgTtft(m), avgTps(m), tpotAvg(m), ok ? ct / ok : NaN,
        truncPct(m), reasonPct(m), e],
      cells: [`<i class="sw" data-style="background:${publisher(m).color}"></i>${escapeHtml(m)}`,
        fmt(reqModels.get(m) || 0), fmt(ct),
        ttftMC.get(m) ? secs(avgTtft(m)) : '–', tpsMC.get(m) ? fmt(avgTps(m)) : '–',
        isFinite(tpotAvg(m)) ? secs(tpotAvg(m)) : '–', ok ? fmt(ct / ok) + ' tok' : '–',
        `<span class="${truncPct(m) >= 1 ? '' : 'dim'}">${pct(truncPct(m))}</span>`,
        `<span class="${reasonPct(m) >= 1 ? '' : 'dim'}">${pct(reasonPct(m))}</span>`,
        `<span class="${isFinite(e) && e > 0 ? 'critc' : 'dim'}">${errPct(m)}</span>`],
    };
  }), { defaultIdx: 2, maxH: 440, minW: 900 });
}

/* ===== clients: what each agent is doing ===== */
function renderClients(c) {
  const { clients, cReq, cC, wsum, streamT, streamF, totStreamT, totGen, totToolReq } = c;
  const avgByKey = (sumN, cntN) => {
    const s = wgroups(sumN, 'client'), n = wgroups(cntN, 'client');
    return cl => n.get(cl) ? s.get(cl) / n.get(cl) : NaN;
  };
  const toolsAvg = avgByKey('nimproxy_request_tools_sum', 'nimproxy_request_tools_count');
  const msgsAvg = avgByKey('nimproxy_request_messages_sum', 'nimproxy_request_messages_count');
  const tempAvg = avgByKey('nimproxy_request_temperature_sum', 'nimproxy_request_temperature_count');
  const genReq = cl => (streamT.get(cl) || 0) + (streamF.get(cl) || 0);
  const streamPct = cl => genReq(cl) ? (streamT.get(cl) || 0) / genReq(cl) * 100 : NaN;
  const oTools = wsum('nimproxy_request_tools_sum'), oToolsN = wsum('nimproxy_request_tools_count');
  const oMsgs = wsum('nimproxy_request_messages_sum'), oMsgsN = wsum('nimproxy_request_messages_count');
  const tile = (label, v, unit) => `<div class="tile"><div class="tlabel">${escapeHtml(label)}</div><div class="tval">${escapeHtml(v)}${unit ? `<span class="unit">${escapeHtml(unit)}</span>` : ''}</div></div>`;
  $('h-kpis').innerHTML =
    tile(catalogMessage('dashboard.clients.tile.clients'), clients.length) +
    tile(catalogMessage('dashboard.clients.tile.streaming_share'), totGen ? pctOf(totStreamT / totGen, 0) : NO_VALUE) +
    tile(catalogMessage('dashboard.clients.tile.avg_tools'), oToolsN ? fmt(oTools / oToolsN) : '–') +
    tile(catalogMessage('dashboard.clients.tile.avg_messages'), oMsgsN ? fmt(oMsgs / oMsgsN) : '–') +
    tile(catalogMessage('dashboard.clients.tile.requests_using_tools'), totGen ? pctOf(totToolReq / totGen, 0) : NO_VALUE);
  barList($('h-toolint'), clients.map(cl => ({ name: cl, v: toolsAvg(cl), color: clientColor(cl) })), fmt);
  barList($('h-depth'), clients.map(cl => ({ name: cl, v: msgsAvg(cl), color: clientColor(cl) })), fmt);
  barList($('h-temp'), clients.map(cl => ({ name: cl, v: tempAvg(cl), color: clientColor(cl) })), v => NUM_2DP.format(v));
  barList($('h-streammix'), clients.map(cl => ({ name: cl, v: streamPct(cl), color: clientColor(cl) })), v => Math.round(v) + '%', 100);
  /* requested output budget — avg of the max_tokens the client asked for.
     Hidden until at least one client sent a cap. */
  const maxTokAvg = avgByKey('nimproxy_request_max_tokens_sum', 'nimproxy_request_max_tokens_count');
  const maxTokN = wgroups('nimproxy_request_max_tokens_count', 'client');
  const maxTokClients = clients.filter(cl => (maxTokN.get(cl) || 0) > 0);
  $('h-maxtok-card').hidden = !maxTokClients.length;
  if (maxTokClients.length)
    barList($('h-maxtok'), maxTokClients.map(cl => ({ name: cl, v: maxTokAvg(cl), color: clientColor(cl) })), fmt);
  sortTable($('table-harness'), 'harness', [
    { label: catalogMessage('dashboard.common.col.client'), align: 'l', str: true }, { label: catalogMessage('dashboard.common.col.requests') }, { label: catalogMessage('dashboard.common.col.tokens_out') },
    { label: 'Tools/req' }, { label: 'Msgs/req' }, { label: catalogMessage('dashboard.common.col.streaming') },
  ], clients.map(cl => ({
    vals: [cl, cReq.get(cl) || 0, cC.get(cl) || 0, toolsAvg(cl), msgsAvg(cl), streamPct(cl)],
    cells: [`<i class="sw" data-style="background:${clientColor(cl)}"></i>${escapeHtml(cl)}`,
      fmt(cReq.get(cl) || 0), fmt(cC.get(cl) || 0),
      isFinite(toolsAvg(cl)) ? fmt(toolsAvg(cl)) : '–', isFinite(msgsAvg(cl)) ? fmt(msgsAvg(cl)) : '–',
      isFinite(streamPct(cl)) ? Math.round(streamPct(cl)) + '%' : '–'],
  })), { defaultIdx: 2, maxH: 420, minW: 620 });
}

/* ===== reliability (was Proxy) ===== */
function renderReliability(c) {
  const { rows, windowLabel, reqPts, wreq, wok, okRatio, clients, cReq, cP, cC,
    shed, unauth, logins, cooldown429, cooldownOther, qwS, qwN, upS, upN, ttftS, ttftN, statusG,
    totStreamT, totStreamF, totGen, totToolReq, wsum } = c;

  /* hero: availability vs SLO + error budget */
  const success = [...statusG.entries()]
    .filter(([status]) => IS_2XX(status))
    .reduce((n, [, value]) => n + value, 0);
  const eligible = [...statusG.entries()]
    .filter(([status]) => status !== 'disconnect' && !/^4\d\d$/.test(status))
    .reduce((n, [, value]) => n + value, 0);
  const slo = (nowData?.slo_target_percent ?? 99.9) / 100;
  const availability = eligible ? success / eligible : NaN;
  const availTxt = isFinite(availability)
    ? pctOf(availability, availability < 0.9995 ? 2 : 3)
    : '–';
  const met = isFinite(availability) && availability >= slo;
  /* Three whole sentences, not "SLO {target} · " + a status word: the target
     and the verdict swap order in plenty of languages, and a fragment gives a
     translator nothing to move. 'SLO' is frozen. */
  const sloLine = (target, hasTraffic, isMet) => {
    const p = { target: (target * 100).toFixed(1) + '%' };
    if (!hasTraffic) return catalogMessage('dashboard.reliability.slo.no_traffic', p);
    return isMet ? catalogMessage('dashboard.reliability.slo.met', p)
                 : catalogMessage('dashboard.reliability.slo.missed', p);
  };
  const budget = !isFinite(availability) ? 0
    : slo >= 1 ? (availability >= 1 ? 0 : 100)
    : Math.min(100, (1 - availability) / (1 - slo) * 100);
  $('r-avail').innerHTML =
    `<div class="hlabel">${escapeHtml(catalogMessage('dashboard.reliability.hero.availability'))} <span class="note">${escapeHtml(windowLabel)}</span></div>
    <div class="hbig" data-style="color:${met ? 'var(--green-lt)' : 'var(--red)'};margin-top:8px">${availTxt}</div>
    <div data-style="font:400 11px var(--mono);color:var(--ink-3);margin-top:2px">${escapeHtml(sloLine(slo, eligible, met))}</div>
    <div data-style="display:flex;justify-content:space-between;font-size:11.5px;color:var(--ink-25);margin:16px 0 6px">${escapeHtml(catalogMessage('dashboard.reliability.hero.error_budget'))}<span data-style="font:500 11px var(--mono);color:var(--ink-2)">${escapeHtml(catalogMessage('dashboard.reliability.hero.budget_used', { pct: budget.toFixed(0) + '%' }))}</span></div>
    <div class="htrack"><span data-style="width:${budget.toFixed(0)}%;background:${budget >= 100 ? 'var(--red)' : 'var(--amber)'}"></span></div>`;

  /* hero: where time goes (median-ish composition from windowed averages) */
  const qw = qwN ? qwS / qwN : 0, ttft = ttftN ? ttftS / ttftN : NaN, up = upN ? upS / upN : NaN;
  if (!upN) {
    $('r-time').innerHTML = `<div class="hlabel">${escapeHtml(catalogMessage('dashboard.reliability.latency.title'))} <span class="note">${escapeHtml(catalogMessage('dashboard.reliability.latency.note'))}</span></div><div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_completed_requests'))}</div>`;
  } else {
    const gen = Math.max(0, up - (isFinite(ttft) ? ttft : 0));
    const segs = [
      { label: catalogMessage('dashboard.reliability.col.queue_wait'), v: qw, color: 'var(--amber)' },
      { label: catalogMessage('dashboard.reliability.col.first_token'), v: isFinite(ttft) ? ttft : 0, color: 'var(--blue)' },
      { label: catalogMessage('dashboard.reliability.col.generation'), v: gen, color: 'var(--green)' },
    ].filter(s => s.v > 0);
    const tot = segs.reduce((a, s) => a + s.v, 0) || 1;
    $('r-time').innerHTML =
      `<div data-style="display:flex;align-items:baseline;gap:10px"><span class="hlabel">${escapeHtml(catalogMessage('dashboard.reliability.latency.title'))}</span>
      <span class="note" data-style="font:400 10px var(--mono);color:var(--ink-3)">${escapeHtml(catalogMessage('dashboard.reliability.latency.note'))}</span>
      <span data-style="margin-left:auto;font-size:22px;font-weight:600;letter-spacing:-0.5px">${secs(tot)}</span></div>
      <div class="segbar" data-style="height:16px;margin:16px 0 12px">${segs.map(s => `<div data-style="width:${(s.v / tot * 100).toFixed(1)}%;background:${s.color}"></div>`).join('')}</div>
      <div class="seglegend">${segs.map(s => `<div class="sl"><span class="sw" data-style="background:${s.color}"></span><div><div class="sll">${escapeHtml(s.label)}</div><div class="slv">${secs(s.v)}</div></div></div>`).join('')}</div>`;
  }

  /* hero: live load + error taxonomy */
  const act = sum(rows, 'nimproxy_active_requests'), que = sum(rows, 'nimproxy_queue_depth');
  const errN = Math.max(0, wreq - wok);
  // These labels remain plain until the title= HTML sink escapes them below.
  const TAX = [
    [catalogMessage('dashboard.common.status.rate_limited'), s => s === '429', 'var(--amber)'],
    [catalogMessage('dashboard.common.status.disconnect'), s => s === 'disconnect', 'var(--blue)'],
    [catalogMessage('dashboard.common.status.timeout_stall'), s => s === '504' || s === 'stall', 'var(--red)'],
    [catalogMessage('dashboard.common.status.client_error_4xx'), s => /^4\d\d$/.test(s) && s !== '429', 'var(--violet, #8B7BB8)'],
    ['5xx', s => /^5\d\d$/.test(s) && s !== '504', '#C15F3C'],
    [catalogMessage('dashboard.models.finish.other'), () => true, 'var(--ink-3)'],
  ];
  const taxCounts = []; let seen = new Set([...statusG.keys()].filter(IS_2XX));
  for (const [label, match, color] of TAX) {
    let n = 0;
    for (const [s, v] of statusG) if (!seen.has(s) && match(s)) { n += v; seen.add(s); }
    if (n > 0) taxCounts.push({ label, n, color });
  }
  const taxTot = taxCounts.reduce((a, x) => a + x.n, 0) || 1;
  $('r-load').innerHTML =
    `<div data-style="display:flex;gap:20px">
      <div><div class="tlabel">${escapeHtml(catalogMessage('dashboard.common.row.active_now'))}</div><div data-style="font-size:26px;font-weight:600;color:var(--green)">${fmt(act)}</div></div>
      <div><div class="tlabel">${escapeHtml(catalogMessage('dashboard.common.row.queued'))}</div><div data-style="font-size:26px;font-weight:600;color:var(--amber)">${fmt(que)}</div></div>
    </div>
    <div class="segbar" data-style="height:8px;border-radius:99px;margin:12px 0">${act + que ? `<div data-style="width:${(act / (act + que) * 100)}%;background:var(--green)"></div><div data-style="width:${(que / (act + que) * 100)}%;background:var(--amber)"></div>` : '<div data-style="width:100%;background:var(--track)"></div>'}</div>
    <div data-style="display:flex;justify-content:space-between;align-items:center;margin-top:14px;padding-top:12px;border-top:1px solid var(--hairline)"><span class="tlabel">${escapeHtml(catalogMessage('dashboard.common.row.error_rate'))}</span><span data-style="font:600 15px var(--mono);color:${errN ? 'var(--red)' : 'var(--ink-3)'}">${wreq ? pctOf(errN / wreq, 1) : NO_VALUE}</span></div>
    <div class="segbar" data-style="height:6px;border-radius:99px;margin-top:8px;gap:1px">${taxCounts.length ? taxCounts.map(x => `<div data-style="width:${(x.n / taxTot * 100).toFixed(1)}%;background:${x.color}" title="${escapeHtml(x.label)}: ${fmt(x.n)}"></div>`).join('') : '<div data-style="width:100%;background:var(--track)"></div>'}</div>`;

  lineChart($('chart-reqrate'), [{ name: catalogMessage('dashboard.chart.requests_per_min'), color: MED, pts: reqPts, area: true }], fmt, { height: 150 });
  const outcomeSeries = [
    { name: catalogMessage('dashboard.chart.success'), color: css('--green'), f: l => IS_2XX(l.status) },
    { name: catalogMessage('dashboard.chart.errors'), color: css('--red'), f: l => IS_ERR(l.status) },
    { name: catalogMessage('dashboard.chart.disconnects'), color: css('--amber'), f: l => l.status === 'disconnect' },
  ].map(({ name, color, f }) => ({ name, color, pts: rateSeries(s => sum(s.rows, 'nimproxy_requests_total', f)) }));
  lineChart($('chart-outcomes'), outcomeSeries, fmt, { height: 150 });
  legend($('legend-outcomes'), outcomeSeries);
  /* stacked composition over time — the same taxonomy groups as the hero
     error breakdown above (success prepended), first match wins so each
     status lands in exactly one band. */
  // These become series names; stackChart and legend escape them at HTML sinks.
  const OUTCOMES = [
    [catalogMessage('dashboard.common.status.success'), css('--green'), IS_2XX],
    [catalogMessage('dashboard.common.status.rate_limited'), css('--amber'), s => s === '429'],
    [catalogMessage('dashboard.common.status.disconnect'), css('--blue'), s => s === 'disconnect'],
    [catalogMessage('dashboard.common.status.timeout_stall'), css('--red'), s => s === '504' || s === 'stall'],
    [catalogMessage('dashboard.common.status.client_error_4xx'), 'var(--violet, #8B7BB8)', s => /^4\d\d$/.test(s) && s !== '429'],
    ['5xx', '#C15F3C', s => /^5\d\d$/.test(s) && s !== '504'],
    [catalogMessage('dashboard.models.finish.other'), css('--ink-3'), () => true],
  ];
  const outcomeIdx = s => OUTCOMES.findIndex(([, , m]) => m(s));
  const stackSeries = OUTCOMES
    .map(([name, color], k) => ({ name, color,
      pts: rateSeries(s => sum(s.rows, 'nimproxy_requests_total', l => outcomeIdx(l.status) === k)) }))
    .filter(s => s.pts.some(p => p.v > 0));
  stackChart($('chart-outcome-stack'), stackSeries, fmt, { height: 200 });
  legend($('legend-outcome-stack'), stackSeries);
  const loadSeries = [
    { name: catalogMessage('dashboard.chart.active'), color: css('--green'), pts: samples.map(s => ({ t: s.t, v: sum(s.rows, 'nimproxy_active_requests') })) },
    { name: catalogMessage('dashboard.chart.queued'), color: css('--amber'), pts: samples.map(s => ({ t: s.t, v: sum(s.rows, 'nimproxy_queue_depth') })) },
  ];
  lineChart($('chart-load'), loadSeries, fmt, { height: 150 });
  legend($('legend-load'), loadSeries);
  const qwSeries = quantSeries('nimproxy_queue_wait_seconds');
  lineChart($('chart-qwait'), qwSeries, secs, { height: 150 });
  legend($('legend-qwait'), qwSeries);

  /* heatmap: requests bucketed by local weekday x hour */
  const matrix = DAYS.map(() => Array(24).fill(0));
  let hmax = 0;
  for (let i = 1; i < samples.length; i++) {
    const d = new Date(samples[i].t);
    const day = dayRow(d.getDay());
    const v = Math.max(0, sum(samples[i].rows, 'nimproxy_requests_total') - sum(samples[i-1].rows, 'nimproxy_requests_total'));
    matrix[day][d.getHours()] += v;
    hmax = Math.max(hmax, matrix[day][d.getHours()]);
  }
  heatmap($('heatmap'), matrix, hmax);

  /* model-pressure governor telemetry — surfaces only once NIM has actually
     exhausted a worker or the governor holds a per-model cap */
  const exhausted = windowed(s => sum(s.rows, 'nimproxy_worker_exhausted_total'));
  const govLimits = [...groups(rows, 'nimproxy_model_limit', 'model').entries()].filter(([, cap]) => cap > 0);
  const pcard = $('card-pressure');
  pcard.hidden = !(exhausted > 0 || govLimits.length);
  if (!pcard.hidden) {
    // lineChart's hover tooltip escapes the plain series name itself.
    lineChart($('chart-exhaust'), [{ name: catalogMessage('dashboard.reliability.pressure.series'), color: css('--amber'),
      pts: rateSeries(s => sum(s.rows, 'nimproxy_worker_exhausted_total')), area: 'url(#gMuted)' }], fmt, { height: 130 });
    const inflight = groups(rows, 'nimproxy_model_inflight', 'model');
    barList($('pressure-bars'), govLimits.map(([m, cap]) => ({
      name: m, label: prettyName(m), v: (inflight.get(m) || 0) / cap * 100,
      vtext: `${fmt(inflight.get(m) || 0)} / ${fmt(cap)}`, color: publisher(m).color,
    })), v => Math.round(v) + '%', 100);
  }

  /* non-success outcomes ranked by count */
  // reason() feeds the escaped display cell; the stable status remains the
  // sort key. Ids are spelled out rather than
  // built from the status, which would hide them from the untagged-string lint.
  const REASONS = {
    'disconnect': catalogMessage('dashboard.common.status.client_disconnected'),
    'stall': catalogMessage('dashboard.common.status.stream_stalled'),
    'stream_error': catalogMessage('dashboard.common.status.stream_error'),
    '504': catalogMessage('dashboard.common.status.upstream_timeout_504'),
    '429': catalogMessage('dashboard.common.status.rate_limited_429'),
    '400': catalogMessage('dashboard.common.status.bad_request_400'),
    '401': catalogMessage('dashboard.common.status.unauthorized_401'),
    '403': catalogMessage('dashboard.common.status.forbidden_403'),
    '404': catalogMessage('dashboard.common.status.not_found_404'),
    '502': catalogMessage('dashboard.common.status.bad_gateway_502'),
    '503': catalogMessage('dashboard.common.status.unavailable_503'),
  };
  const reason = s => REASONS[s] || (/^\d+$/.test(s) ? `HTTP ${s}` : s);
  const totReq = [...statusG.values()].reduce((a, b) => a + b, 0) || 1;
  const bad = [...statusG.entries()].filter(([s]) => !IS_2XX(s));
  sortTable($('table-outcomes'), 'outcomes',
    [{ label: catalogMessage('dashboard.common.col.reason'), align: 'l', str: true }, { label: catalogMessage('dashboard.common.col.count') }, { label: catalogMessage('dashboard.common.col.share') }],
    bad.map(([s, n]) => ({
      vals: [s, n, n / totReq * 100],
      cells: [escapeHtml(reason(s)), fmt(n), pctOf(n / totReq, 1)],
    })), { defaultIdx: 1, maxH: 240, empty: catalogMessage('dashboard.reliability.empty.no_errors') });

  $('table-reliability').innerHTML =
    metricRow(catalogMessage('dashboard.common.row.active_now'), fmt(act)) +
    metricRow(catalogMessage('dashboard.common.row.queued'), fmt(que)) +
    metricRow(catalogMessage('dashboard.reliability.row.dropped_503'), fmt(shed), shed > 0 ? 'crit' : 'zero') +
    metricRow(catalogMessage('dashboard.common.row.rate_limit_cooldowns'), fmt(cooldown429), cooldown429 > 0 ? 'warn' : 'zero') +
    metricRow(catalogMessage('dashboard.common.status.upstream_error_cooldowns'), fmt(cooldownOther), cooldownOther > 0 ? 'warn' : 'zero') +
    metricRow(catalogMessage('dashboard.common.status.unauthorized_401'), fmt(unauth), unauth > 0 ? 'crit' : 'zero') +
    metricRow(catalogMessage('dashboard.common.row.failed_logins'), fmt(logins), logins > 0 ? 'crit' : 'zero');
  const jsonMode = windowed(s => sum(s.rows, 'nimproxy_json_mode_total'));
  const tcModes = wgroups('nimproxy_tool_choice_total', 'mode');
  const tcStr = [...tcModes.entries()].sort((a, b) => b[1] - a[1]).map(([m, n]) => `${m} ${fmt(n)}`).join(', ') || '–';
  $('table-reqtypes').innerHTML =
    metricRow(catalogMessage('dashboard.common.row.streaming'), `${fmt(totStreamT)}${totGen ? ` (${Math.round(totStreamT / totGen * 100)}%)` : ''}`) +
    metricRow(catalogMessage('dashboard.reliability.row.buffered'), `${fmt(totStreamF)}${totGen ? ` (${Math.round(totStreamF / totGen * 100)}%)` : ''}`) +
    metricRow(catalogMessage('dashboard.reliability.row.requests_with_tools'), `${fmt(totToolReq)}${totGen ? ` (${Math.round(totToolReq / totGen * 100)}%)` : ''}`) +
    metricRow(catalogMessage('dashboard.reliability.row.json_mode'), fmt(jsonMode)) +
    metricRow(catalogMessage('dashboard.reliability.row.tool_choice'), tcStr);

  sortTable($('table-clients'), 'clients', [
    { label: catalogMessage('dashboard.common.col.client'), align: 'l', str: true }, { label: catalogMessage('dashboard.common.col.requests') }, { label: catalogMessage('dashboard.common.col.tokens_in') },
    { label: catalogMessage('dashboard.common.col.tokens_out') },
  ], clients.map(cl => ({
    vals: [cl, cReq.get(cl) || 0, cP.get(cl) || 0, cC.get(cl) || 0],
    cells: [`<i class="sw" data-style="background:${clientColor(cl)}"></i>${escapeHtml(cl)}`,
      fmt(cReq.get(cl) || 0), fmt(cP.get(cl) || 0), fmt(cC.get(cl) || 0)],
  })), { defaultIdx: 3, maxH: 420, minW: 560 });
}

/* ===== capacity (was Keys) ===== */
const LANE_COLORS = ['#76B900', '#4D6BFE', '#D9A521', '#16B8C0', '#615CED', '#EE6002'];
/* Which plural form a count takes is CLDR data, like the weekday names above:
   Intl.PluralRules already knows it for every locale, so `n === 1 ? '' : 's'`
   would be English grammar hardcoded into the render path. The source catalog
   carries all six categories because locale_v1 requires exact id parity — a
   form absent here can never be added by a translation, and ar/ru/pl/cy need
   categories English does not have. Ids are spelled out rather than built from
   the category, so the lint can still see them. */
const errorCooldownSummary = (share, cooldowns) => {
  const p = { pct: pctOf(share / 100, share && share < 10 ? 1 : 0), cooldowns: fmt(cooldowns) };
  switch (PLURALS.select(cooldowns)) {
    case 'zero': return catalogMessage('dashboard.overview.ring.errors.zero', p);
    case 'one': return catalogMessage('dashboard.overview.ring.errors.one', p);
    case 'two': return catalogMessage('dashboard.overview.ring.errors.two', p);
    case 'few': return catalogMessage('dashboard.overview.ring.errors.few', p);
    case 'many': return catalogMessage('dashboard.overview.ring.errors.many', p);
    default: return catalogMessage('dashboard.overview.ring.errors.other', p);
  }
};
const footerInfoId = (keys, auth) => {
  if (auth) {
    switch (PLURALS.select(keys)) {
      case 'zero': return 'dashboard.nav.footer.api_key_required.zero';
      case 'one': return 'dashboard.nav.footer.api_key_required.one';
      case 'two': return 'dashboard.nav.footer.api_key_required.two';
      case 'few': return 'dashboard.nav.footer.api_key_required.few';
      case 'many': return 'dashboard.nav.footer.api_key_required.many';
      default: return 'dashboard.nav.footer.api_key_required.other';
    }
  }
  switch (PLURALS.select(keys)) {
    case 'zero': return 'dashboard.nav.footer.open.zero';
    case 'one': return 'dashboard.nav.footer.open.one';
    case 'two': return 'dashboard.nav.footer.open.two';
    case 'few': return 'dashboard.nav.footer.open.few';
    case 'many': return 'dashboard.nav.footer.open.many';
    default: return 'dashboard.nav.footer.open.other';
  }
};
const capacityKeySummary = (rpmNow, capacity, n) => {
  const p = { rpm_now: fmt(rpmNow), rpm_capacity: fmt(capacity), n: fmt(n) };
  switch (PLURALS.select(n)) {
    case 'zero': return catalogMessage('dashboard.overview.ring.capacity_sub.zero', p);
    case 'one': return catalogMessage('dashboard.overview.ring.capacity_sub.one', p);
    case 'two': return catalogMessage('dashboard.overview.ring.capacity_sub.two', p);
    case 'few': return catalogMessage('dashboard.overview.ring.capacity_sub.few', p);
    case 'many': return catalogMessage('dashboard.overview.ring.capacity_sub.many', p);
    default: return catalogMessage('dashboard.overview.ring.capacity_sub.other', p);
  }
};
const modelCountId = n => {
  switch (PLURALS.select(n)) {
    case 'zero': return 'dashboard.models.count.zero';
    case 'one': return 'dashboard.models.count.one';
    case 'two': return 'dashboard.models.count.two';
    case 'few': return 'dashboard.models.count.few';
    case 'many': return 'dashboard.models.count.many';
    default: return 'dashboard.models.count.other';
  }
};
const modelCount = n => catalogMessage(modelCountId(n), { n: fmt(n) });
const modelCountWithSortId = n => {
  switch (PLURALS.select(n)) {
    case 'zero': return 'dashboard.models.count_with_sort.zero';
    case 'one': return 'dashboard.models.count_with_sort.one';
    case 'two': return 'dashboard.models.count_with_sort.two';
    case 'few': return 'dashboard.models.count_with_sort.few';
    case 'many': return 'dashboard.models.count_with_sort.many';
    default: return 'dashboard.models.count_with_sort.other';
  }
};
const enabledKeys = n => {
  const p = { n: fmt(n) };
  switch (PLURALS.select(n)) {
    case 'zero': return catalogMessage('dashboard.capacity.note.enabled_keys.zero', p);
    case 'one': return catalogMessage('dashboard.capacity.note.enabled_keys.one', p);
    case 'two': return catalogMessage('dashboard.capacity.note.enabled_keys.two', p);
    case 'few': return catalogMessage('dashboard.capacity.note.enabled_keys.few', p);
    case 'many': return catalogMessage('dashboard.capacity.note.enabled_keys.many', p);
    default: return catalogMessage('dashboard.capacity.note.enabled_keys.other', p);
  }
};
/* Same reasoning as enabledKeys above: `interval${n === 1 ? '' : 's'}` was a
   second copy of English grammar in the render path. */
const unknownIntervals = n => {
  const p = { n: fmt(n) };
  switch (PLURALS.select(n)) {
    case 'zero': return catalogMessage('dashboard.capacity.history.no_data.zero', p);
    case 'one': return catalogMessage('dashboard.capacity.history.no_data.one', p);
    case 'two': return catalogMessage('dashboard.capacity.history.no_data.two', p);
    case 'few': return catalogMessage('dashboard.capacity.history.no_data.few', p);
    case 'many': return catalogMessage('dashboard.capacity.history.no_data.many', p);
    default: return catalogMessage('dashboard.capacity.history.no_data.other', p);
  }
};
function renderCapacity(c) {
  const { capacity, rpmNow, capRatio, capColor, cooldown429, capacityPoints } = c;

  /* hero: current saturation uses only the current `/now` rate and config */
  const satPct = Math.min(1, capRatio);
  const headroom = Math.max(0, capacity - rpmNow);
  $('k-sat').innerHTML =
    `<div data-style="display:flex;align-items:baseline;gap:10px"><span class="hlabel">${escapeHtml(catalogMessage('dashboard.capacity.hero.saturation'))}</span><span class="tag">${escapeHtml(catalogMessage('dashboard.common.tag.now'))}</span>
    <span class="note" data-style="font:400 10px var(--mono);color:var(--ink-3)">${escapeHtml(catalogMessage('dashboard.capacity.note.current_vs_capacity'))}</span>
    <span data-style="margin-left:auto;font-size:30px;font-weight:600;letter-spacing:-1px;color:${capColor}">${Math.round(satPct * 100)}%</span></div>
    <div data-style="position:relative;height:20px;background:var(--track);border-radius:6px;overflow:hidden;margin:16px 0 8px">
      <div data-style="position:absolute;inset:0;width:${(satPct * 100).toFixed(1)}%;background:linear-gradient(90deg,var(--green-dk),var(--green));border-radius:6px"></div>
    </div>
    <div data-style="display:flex;justify-content:space-between;font:400 11px var(--mono);color:var(--ink-3)">
      <span data-style="color:var(--ink-2)">${fmt(rpmNow)} / ${fmt(capacity)} rpm</span>
      <span>${escapeHtml(enabledKeys(cfg.lanes))}</span>
      <span data-style="color:var(--green-lt)">${fmt(headroom)} ${escapeHtml(catalogMessage('dashboard.capacity.note.rpm_available'))}</span>
    </div>`;

  /* Historical comparisons use each bucket's capacity at the time.
     Legacy buckets without metadata never inherit today's configuration. */
  const knownCapacity = capacityPoints.filter(point => point.capacity != null);
  const unknownCapacity = capacityPoints.length - knownCapacity.length;
  if (!knownCapacity.length) {
    $('k-prov').innerHTML =
      `<div class="hlabel">${escapeHtml(catalogMessage('dashboard.capacity.history.title'))}</div>
      <div class="empty">${escapeHtml(catalogMessage('dashboard.capacity.empty.no_capacity_data'))}</div>`;
  } else {
    const weightedSeconds = knownCapacity.reduce((n, point) => n + point.duration, 0);
    const averageUtil = weightedSeconds
      ? knownCapacity.reduce((n, point) => n + point.rpm / point.capacity * point.duration, 0) / weightedSeconds
      : NaN;
    const peak = knownCapacity.reduce((best, point) =>
      point.rpm / point.capacity > best.rpm / best.capacity ? point : best);
    const shortfall = knownCapacity.reduce((best, point) =>
      point.rpm - point.capacity > best.rpm - best.capacity ? point : best);
    const shortRpm = Math.max(0, Math.ceil(shortfall.rpm - shortfall.capacity));
    $('k-prov').innerHTML =
      `<div class="hlabel">${escapeHtml(catalogMessage('dashboard.capacity.history.title'))}</div>
      <div class="capacity-history-shortfall">
        <span class="capacity-history-value" data-style="color:${shortRpm > 0 ? 'var(--amber)' : 'var(--ink-1)'}">${fmt(shortRpm)}</span>
        <span class="capacity-history-label">${escapeHtml(catalogMessage('dashboard.capacity.history.shortfall'))}</span></div>
      <div class="capacity-history-utilization">
        ${escapeHtml(catalogMessage('dashboard.capacity.history.utilization', {
          avg: isFinite(averageUtil) ? pctOf(averageUtil, 0) : NO_VALUE,
          peak: pctOf(peak.rpm / peak.capacity, 0),
        }))}${unknownCapacity ? ' · ' + escapeHtml(unknownIntervals(unknownCapacity)) : ''}
      </div>`;
  }

  /* hero: rate-limit pressure */
  const sticky = windowed(s => sum(s.rows, 'nimproxy_affinity_total', l => l.result === 'sticky'));
  const spill = windowed(s => sum(s.rows, 'nimproxy_affinity_total', l => l.result === 'spill'));
  const stickyTxt = sticky + spill ? Math.round(sticky / (sticky + spill) * 100) + '%' : '–';
  const prow = (l, v, color) => `<div data-style="display:flex;justify-content:space-between;align-items:center;padding:6px 0"><span data-style="font-size:12.5px;color:var(--ink-2)">${escapeHtml(l)}</span><span data-style="font:600 15px var(--mono);color:${color || 'var(--ink-1)'}">${escapeHtml(v)}</span></div>`;
  $('k-pressure').innerHTML =
    `<div class="hlabel" data-style="margin-bottom:12px">${escapeHtml(catalogMessage('dashboard.capacity.hero.throttling'))}</div>` +
    prow(catalogMessage('dashboard.common.status.rate_limited_429'), fmt(cooldown429), cooldown429 > 0 ? 'var(--amber)' : 'var(--ink-3)') +
    prow(catalogMessage('dashboard.capacity.row.session_affinity'), stickyTxt) +
    prow(catalogMessage('dashboard.capacity.row.slots_in_use'), cfg.lanes);

  /* Current per-slot utilization comes from adjacent `/now` polls. Selected
     window counts remain selected-window values; neither domain is subtracted
     from the other. Slot numbers are explicitly not durable key identities. */
  const laneWindow = wgroups('nimproxy_lane_requests_total', 'lane');
  const lane429 = wgroups('nimproxy_lane_cooldown_total', 'lane', l => l.status === '429');
  const laneOther = wgroups('nimproxy_lane_cooldown_total', 'lane', l => l.status !== '429');
  const totalLane = [...laneWindow.values()].reduce((a, b) => a + b, 0) || 1;
  const laneColor = i => LANE_COLORS[i % LANE_COLORS.length];
  const lanes = Array.from({ length: cfg.lanes }, (_, i) => {
    const k = String(i);
    const currentRpm = nowLaneRates.get(k) || 0;
    return { i, name: catalogMessage('dashboard.capacity.slot', { n: fmt(i + 1) }), cur: laneWindow.get(k) || 0, currentRpm,
      util: Math.min(1, currentRpm / Math.max(1, laneRpm(i))),
      b429: lane429.get(k) || 0, bOther: laneOther.get(k) || 0 };
  });
  if (!lanes.length) $('meters').innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_keys_configured'))}</div>`;
  else barList($('meters'), lanes.map(l => ({
    name: l.name, v: l.util * 100, color: l.util >= 0.9 ? css('--amber') : laneColor(l.i),
  })), v => Math.round(v) + '%', 100);

  const cooldownSeries = lanes.slice(0, 6).map(l => ({
    name: l.name, color: laneColor(l.i),
    pts: rateSeries(s => sum(s.rows, 'nimproxy_lane_cooldown_total', ll => ll.lane === String(l.i) && ll.status === '429')),
  }));
  lineChart($('chart-cooldowns'), cooldownSeries, fmt, { height: 150 });
  legend($('legend-cooldowns'), cooldownSeries);

  sortTable($('table-lanes'), 'lanes', [
    { label: catalogMessage('dashboard.common.col.key'), align: 'l', str: true }, { label: catalogMessage('dashboard.common.col.requests') }, { label: catalogMessage('dashboard.common.col.share') },
    { label: catalogMessage('dashboard.capacity.col.current_rate') }, { label: catalogMessage('dashboard.capacity.col.429s') }, { label: catalogMessage('dashboard.common.status.upstream_error_cooldowns') },
  ], lanes.map(l => ({
    vals: [l.name, l.cur, l.cur / totalLane * 100, l.currentRpm, l.b429, l.bOther],
    cells: [`<i class="sw" data-style="background:${laneColor(l.i)}"></i>${escapeHtml(l.name)}`,
      fmt(l.cur), pctOf(l.cur / totalLane, 0), `${fmt(l.currentRpm)} / ${laneRpm(l.i)}`,
      `<span class="${l.b429 ? 'warnc' : 'dim'}">${fmt(l.b429)}</span>`,
      `<span class="${l.bOther ? 'critc' : 'dim'}">${fmt(l.bOther)}</span>`],
  })), { defaultIdx: 1, maxH: 360, minW: 560, empty: catalogMessage('dashboard.common.empty.no_keys_configured') });
}

const RENDERERS = { overview: renderOverview, models: renderModels, clients: renderClients,
  reliability: renderReliability, capacity: renderCapacity,
  settings: () => {} /* settings never rides the metrics poll */ };

/* ---------- data sources ---------- */
async function fetchRange(from, to) {
  const generation = ++rangeRequestGeneration;
  const url = `/api/dashboard?from=${from}&to=${to}&points=288`;
  try {
    const response = await fetch(url);
    if (generation !== rangeRequestGeneration) return false;
    if (!response.ok) throw new Error(`range ${response.status}`);
    const next = await response.json();
    if (generation !== rangeRequestGeneration) return false;
    rangeData = next;
    return true;
  } catch (error) {
    if (generation !== rangeRequestGeneration) return false;
    throw error;
  }
}

function applyNowConfig(next) {
  cfg = {
    version: next.version || '', started: +next.started || Date.now()/1000,
    lanes: +next.lanes || 0, rpms: next.rpms || [], rpm: Math.max(0, ...(next.rpms || [40])),
    capacity_rpm: +next.capacity_rpm || 0,
    auth: !!next.auth,
    default_window_days: +next.default_window_days || 30,
    retention_days: +next.retention_days || 0,
    slo_target_percent: +next.slo_target_percent || 99.9,
  };
  setMessageText($('verinfo'), footerInfoId(cfg.lanes, cfg.auth), {
    version: cfg.version, keys: fmt(cfg.lanes),
  });
  const defaultButton = document.querySelector('#ranges [data-range="default"]');
  if (defaultButton) setMessageText(defaultButton, 'dashboard.topbar.range.default_dynamic', { days: fmt(cfg.default_window_days) });
}

function followingBounds() {
  const to = nowData.sampled_at;
  if (mode.preset === 'all-retained') {
    const earliest = nowData.available_from;
    return { from: earliest == null ? Math.max(0, to - 1) : Math.max(0, earliest - 1), to };
  }
  const seconds = mode.preset === 'default'
    ? cfg.default_window_days * 86400
    : Math.max(1, parseInt(mode.preset));
  return { from: Math.max(0, to - seconds), to };
}

const presetLabel = () => document.querySelector(`#ranges [data-range="${mode.preset}"]`)?.textContent || '';

const scopeDate = seconds => at(DAY_SHORT, seconds * 1000);
const scopeTime = seconds => at(SCOPE_TIME, seconds * 1000);

function updateScope() {
  if (!rangeData) return;
  const tail = mode.kind === 'following' && !mode.paused &&
    nowData?.tail?.base_history_revision === rangeData.history_revision ? nowData.tail : null;
  const window = rangeData.window;
  const hasEffectiveHistory = window.effective_from != null && window.effective_to != null
    && rangeData.points.length > 0;
  if (!window.complete) {
    if (hasEffectiveHistory) {
      setMessageText($('rangeinfo'), 'dashboard.topbar.scope.partial_history', {
        from: scopeDate(window.effective_from), to: scopeDate(window.effective_to),
      });
      return;
    }
    if (window.available_from != null && window.available_to != null
      && window.requested_from >= window.available_from && window.requested_to <= window.available_to) {
      setMessageText($('rangeinfo'), 'dashboard.common.empty.history_unavailable');
      return;
    }
  }
  if (window.available_from == null) {
    setMessageText($('rangeinfo'), 'dashboard.common.empty.no_data_yet');
    return;
  }
  const hasTraffic = mode.paused ? frozenHasTraffic : hasSelectedRequestTraffic(samples);
  if (!hasTraffic) {
    setMessageText($('rangeinfo'), 'dashboard.common.empty.no_data_in_range');
    return;
  }
  const from = mode.paused ? mode.from : window.effective_from ?? mode.from;
  const to = mode.paused ? mode.to : tail?.to ?? window.effective_to ?? mode.to;
  if (mode.kind === 'following' && !mode.paused) {
    setMessageText($('rangeinfo'), 'dashboard.topbar.scope.following', {
      preset: presetLabel(), from: scopeDate(from), to: scopeDate(to),
    });
  } else {
    setMessageText($('rangeinfo'), 'dashboard.topbar.scope.absolute', { from: scopeTime(from), to: scopeTime(to) });
  }
}

function syncFollowControl(offline = false) {
  if (offline) {
    $('live').className = 'live down';
    setMessageText($('liveText'), 'dashboard.nav.live.disconnected');
    return;
  }
  const following = mode.kind === 'following' && !mode.paused;
  $('live').className = following ? 'live' : 'live idle';
  if (following) setMessageText($('liveText'), 'dashboard.nav.live.following');
  else setMessageText($('liveText'), 'dashboard.nav.live.frozen');
}

function rebuildSamples() {
  if (!rangeData) return;
  const tail = mode.kind === 'following' && !mode.paused ? nowData?.tail : null;
  samples = rangeSamples(rangeData, tail);
  updateScope();
}

async function refreshSelectedRange() {
  if (!nowData) return;
  if (mode.kind === 'following') {
    const bounds = followingBounds();
    mode.from = bounds.from;
    mode.to = bounds.to;
  }
  const loaded = await fetchRange(mode.from, mode.to);
  if (!loaded) return false;
  rebuildSamples();
  return true;
}

let pollingNow = false;
async function pollNow() {
  if (pollingNow) return;
  pollingNow = true;
  try {
    const response = await fetch('/api/dashboard/now');
    if (!response.ok) throw new Error(`now ${response.status}`);
    const next = await response.json();
    const nextRows = next.metrics.map(asRow);
    const historyChanged = rangeData && next.history_revision !== rangeData.history_revision;
    const configChanged = nowData && next.config_revision !== nowData.config_revision;

    if (previousNow) {
      nowRpm = rate(sum(nextRows, 'nimproxy_requests_total'),
        sum(previousNow.rows, 'nimproxy_requests_total'),
        next.sampled_at * 1000, previousNow.t);
      if (configChanged) {
        nowLaneRates = new Map();
      } else {
        const currentLanes = groups(nextRows, 'nimproxy_lane_requests_total', 'lane');
        const previousLanes = groups(previousNow.rows, 'nimproxy_lane_requests_total', 'lane');
        nowLaneRates = new Map([...new Set([...currentLanes.keys(), ...previousLanes.keys()])].map(lane =>
          [lane, rate(currentLanes.get(lane) || 0, previousLanes.get(lane) || 0,
            next.sampled_at * 1000, previousNow.t)]));
      }
    }
    previousNow = { t: next.sampled_at * 1000, rows: nextRows };
    nowData = next;
    nowRows = nextRows;
    applyNowConfig(next);

    if (!rangeData) {
      await refreshSelectedRange();
    } else if (mode.kind === 'following' && !mode.paused) {
      if (historyChanged || (configChanged && mode.preset === 'default'))
        await refreshSelectedRange();
      else {
        const bounds = followingBounds();
        mode.from = bounds.from;
        mode.to = bounds.to;
        rebuildSamples();
      }
    }
    render();
    syncFollowControl();
    $('uptime').textContent = 'up ' + ago(next.sampled_at - cfg.started);
  } catch {
    syncFollowControl(true);
  } finally {
    pollingNow = false;
  }
}

/* ---------- controls ---------- */
for (const b of document.querySelectorAll('#ranges button')) {
  b.addEventListener('click', async () => {
    for (const o of document.querySelectorAll('#ranges button')) o.setAttribute('aria-pressed', o === b);
    const r = b.dataset.range;
    $('custom').classList.toggle('show', r === 'custom');
    if (r !== 'custom') {
      mode = { kind: 'following', preset: r, paused: false, from: 0, to: 0 };
      frozenHasTraffic = false;
      syncFollowControl();
      try {
        if (await refreshSelectedRange()) render();
      } catch {
        setMessageText($('rangeinfo'), 'dashboard.topbar.error.load');
      }
    }
  });
}
$('applyRange').addEventListener('click', async () => {
  const f = new Date($('from').value).getTime()/1000, to = new Date($('to').value).getTime()/1000;
  if (!isFinite(f) || !isFinite(to) || to <= f) return;
  mode = { kind: 'fixed', preset: 'custom', paused: false, from: Math.floor(f), to: Math.floor(to) };
  frozenHasTraffic = false;
  syncFollowControl();
  try {
    if (await refreshSelectedRange()) render();
  } catch {
    setMessageText($('rangeinfo'), 'dashboard.topbar.error.load');
  }
});
$('live').addEventListener('click', async () => {
  if (mode.kind !== 'following' || !rangeData || !samples.length) return;
  mode.paused = !mode.paused;
  if (mode.paused && samples.length) {
    rangeRequestGeneration++;
    frozenHasTraffic = hasSelectedRequestTraffic(samples);
    mode.from = samples[0].t / 1000;
    mode.to = samples[samples.length - 1].t / 1000;
    updateScope();
    render();
  } else if (!mode.paused) {
    frozenHasTraffic = false;
    try {
      if (await refreshSelectedRange()) render();
    } catch {
      setMessageText($('rangeinfo'), 'dashboard.topbar.error.resume');
    }
  }
  syncFollowControl();
});
/* Static text first: the restore loop below sets #pagetitle, and applying
   the catalog afterwards would overwrite it with the Overview default. */
applyStatic(document);
for (const b of document.querySelectorAll('#side nav button')) {
  b.addEventListener('click', () => {
    for (const o of document.querySelectorAll('#side nav button')) {
      if (o === b) o.setAttribute('aria-current', 'page');
      else o.removeAttribute('aria-current');
    }
    for (const s of document.querySelectorAll('main > section')) s.hidden = s.id !== 'tab-' + b.dataset.tab;
    activeTab = b.dataset.tab;
    setMessageText($('pagetitle'), TITLES[activeTab]);
    // Settings is config-driven: hide the metrics range picker and (re)fetch.
    document.body.classList.toggle('on-settings', activeTab === 'settings');
    if (activeTab === 'settings') loadSettings();
    location.hash = activeTab;
    render();
  });
  if (location.hash === '#' + b.dataset.tab) b.click();
}
document.body.hidden = false;

/* ---------- boot ---------- */
(async () => {
  const pad = n => String(n).padStart(2, '0');
  const toLocal = d => `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  $('to').value = toLocal(new Date());
  $('from').value = toLocal(new Date(Date.now() - 86400e3));
  await pollNow();
  setInterval(pollNow, POLL_MS);
})();
