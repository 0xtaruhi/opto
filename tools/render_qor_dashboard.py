#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Append a public QoR result and render a self-contained trend dashboard."""

import argparse
import datetime as dt
import html
import json
from pathlib import Path


def metric(tool, name):
    return None if tool is None else tool.get(name)


def point_from_result(document, commit, timestamp):
    cases = []
    for result in document["results"]:
        opto = result.get("opto")
        yosys = result.get("yosys_abc")
        timing = None if opto is None else opto.get("timing")
        yosys_timing = None if yosys is None else yosys.get("timing")
        cases.append(
            {
                "id": result["id"],
                "status": result["status"],
                "diagnostics": result.get("diagnostics", []),
                "equivalence": result["equivalence"],
                "opto_area": metric(opto, "area"),
                "opto_cells": metric(opto, "cells"),
                "opto_cell_histogram": metric(opto, "cell_histogram"),
                "opto_wall_seconds": metric(
                    None if opto is None else opto.get("metrics"), "wall_seconds"
                ),
                "opto_cpu_seconds": metric(
                    None if opto is None else opto.get("metrics"), "cpu_seconds"
                ),
                "opto_peak_rss_kib": metric(
                    None if opto is None else opto.get("metrics"), "peak_rss_kib"
                ),
                "critical_delay": metric(timing, "critical_delay"),
                "worst_slack": metric(timing, "worst_slack"),
                "total_negative_slack": metric(timing, "total_negative_slack"),
                "violating_paths": metric(timing, "violating_paths"),
                "yosys_area": metric(yosys, "area"),
                "yosys_cells": metric(yosys, "cells"),
                "yosys_cell_histogram": metric(yosys, "cell_histogram"),
                "yosys_wall_seconds": metric(
                    None if yosys is None else yosys.get("metrics"), "wall_seconds"
                ),
                "yosys_cpu_seconds": metric(
                    None if yosys is None else yosys.get("metrics"), "cpu_seconds"
                ),
                "yosys_peak_rss_kib": metric(
                    None if yosys is None else yosys.get("metrics"), "peak_rss_kib"
                ),
                "yosys_critical_delay": metric(yosys_timing, "critical_delay"),
                "yosys_worst_slack": metric(yosys_timing, "worst_slack"),
                "yosys_total_negative_slack": metric(
                    yosys_timing, "total_negative_slack"
                ),
                "yosys_violating_paths": metric(yosys_timing, "violating_paths"),
            }
        )
    return {
        "timestamp": timestamp,
        "commit": commit,
        "suite": document["suite"],
        "cases": cases,
    }


def load_history(path):
    if not path.exists():
        return {"format": 1, "points": []}
    history = json.loads(path.read_text(encoding="utf-8"))
    if history.get("format") != 1 or not isinstance(history.get("points"), list):
        raise ValueError(f"unsupported QoR history format in {path}")
    return history


def validate_result_document(document):
    if document.get("format") != 1:
        raise ValueError("unsupported QoR result format")
    if not isinstance(document.get("suite"), str) or not document["suite"]:
        raise ValueError("QoR result has no suite name")
    results = document.get("results")
    if not isinstance(results, list) or not results:
        raise ValueError("QoR result has no cases")
    for result in results:
        if result.get("status") not in {"pass", "fail"}:
            raise ValueError(f"invalid status for QoR case {result.get('id')!r}")
        if result["status"] == "fail" and not result.get("diagnostics"):
            raise ValueError(f"failed QoR case {result.get('id')!r} has no diagnostics")


def render(history):
    payload = json.dumps(history, separators=(",", ":")).replace("</", "<\\/")
    latest = history["points"][-1]
    title = f"Opto QoR trends — {latest['suite']}"
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{html.escape(title)}</title>
<style>
:root {{ color-scheme: light dark; font-family: system-ui, sans-serif; }}
body {{ max-width: 1180px; margin: 2rem auto; padding: 0 1rem; }}
h1 {{ margin-bottom: .25rem; }} .muted {{ color: #777; }}
.case {{ border: 1px solid #8885; border-radius: .6rem; padding: 1rem; margin: 1rem 0; overflow-x: auto; }}
.charts {{ display: grid; grid-template-columns: repeat(auto-fit,minmax(300px,1fr)); gap: 1rem; }}
svg {{ width: 100%; height: 150px; overflow: visible; }}
.axis {{ stroke: #8888; }} .opto {{ fill: none; stroke: #08c; stroke-width: 2; }}
.yosys {{ fill: none; stroke: #e65; stroke-width: 2; }} .slack {{ fill: none; stroke: #3a5; stroke-width: 2; }}
table {{ border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; }}
th,td {{ padding: .35rem .5rem; border-bottom: 1px solid #8884; text-align: right; }}
th:first-child,td:first-child {{ text-align: left; }}
.legend span {{ margin-right: 1rem; }} .ok {{ color: #287d3c; }} .bad {{ color: #b33; }}
</style>
</head>
<body>
<h1>{html.escape(title)}</h1>
<p class="muted">Generated from reproducible weekly public-library runs. Latest commit <code>{html.escape(latest['commit'][:12])}</code>.</p>
<main id="dashboard"></main>
<script>
const history={payload};
const points=history.points;
const ids=[...new Set(points.flatMap(p=>p.cases.map(c=>c.id)))].sort();
const value=(p,id,key)=>p.cases.find(c=>c.id===id)?.[key] ?? null;
const fmt=v=>v==null?'—':Number(v).toLocaleString(undefined,{{maximumFractionDigits:3}});
const esc=v=>String(v).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;').replaceAll("'",'&#39;');
const histogram=v=>v==null?'—':`<code>${{esc(JSON.stringify(v))}}</code>`;
function chart(id, primary, secondary, css1, css2) {{
  const series=[primary,secondary].filter(Boolean).map(key=>points.map((p,i)=>[i,value(p,id,key)]).filter(x=>x[1]!=null));
  const values=series.flatMap(s=>s.map(x=>x[1]));
  if (!values.length) return '<p class="muted">No measurements</p>';
  let lo=Math.min(...values), hi=Math.max(...values); if (lo===hi) {{ lo-=1; hi+=1; }}
  const xy=([i,v])=>`${{10+(points.length===1?0:i*280/(points.length-1))}},${{135-(v-lo)*120/(hi-lo)}}`;
  const paths=series.map((s,i)=>`<polyline class="${{i?css2:css1}}" points="${{s.map(xy).join(' ')}}"/>`).join('');
  return `<svg viewBox="0 0 300 150" role="img"><line class="axis" x1="10" y1="135" x2="290" y2="135"/>${{paths}}<text x="12" y="14">${{fmt(hi)}}</text><text x="12" y="132">${{fmt(lo)}}</text></svg>`;
}}
const root=document.querySelector('#dashboard');
for (const id of ids) {{
  const rows=points.map(p=>{{
    const status=value(p,id,'status');
    const diagnostics=value(p,id,'diagnostics') ?? [];
    const detail=diagnostics.length ? ` title="${{esc(diagnostics.join('; '))}}"` : '';
    const statusClass=status==='pass'?'ok':status==='fail'?'bad':'';
    return `<tr><td>${{esc(p.timestamp.slice(0,10))}}</td><td><code>${{esc(p.commit.slice(0,8))}}</code></td><td class="${{statusClass}}"${{detail}}>${{esc(status ?? '—')}}</td><td>${{esc(value(p,id,'equivalence') ?? '—')}}</td><td>${{fmt(value(p,id,'opto_area'))}}</td><td>${{fmt(value(p,id,'yosys_area'))}}</td><td>${{fmt(value(p,id,'opto_cells'))}}</td><td>${{fmt(value(p,id,'yosys_cells'))}}</td><td>${{fmt(value(p,id,'critical_delay'))}}</td><td>${{fmt(value(p,id,'yosys_critical_delay'))}}</td><td>${{fmt(value(p,id,'worst_slack'))}}</td><td>${{fmt(value(p,id,'yosys_worst_slack'))}}</td><td>${{fmt(value(p,id,'total_negative_slack'))}}</td><td>${{fmt(value(p,id,'yosys_total_negative_slack'))}}</td><td>${{fmt(value(p,id,'violating_paths'))}}</td><td>${{fmt(value(p,id,'yosys_violating_paths'))}}</td><td>${{fmt(value(p,id,'opto_wall_seconds'))}}</td><td>${{fmt(value(p,id,'yosys_wall_seconds'))}}</td><td>${{fmt(value(p,id,'opto_cpu_seconds'))}}</td><td>${{fmt(value(p,id,'yosys_cpu_seconds'))}}</td><td>${{fmt(value(p,id,'opto_peak_rss_kib'))}}</td><td>${{fmt(value(p,id,'yosys_peak_rss_kib'))}}</td><td>${{histogram(value(p,id,'opto_cell_histogram'))}}</td><td>${{histogram(value(p,id,'yosys_cell_histogram'))}}</td></tr>`;
  }}).join('');
  root.insertAdjacentHTML('beforeend',`<section class="case"><h2>${{esc(id)}}</h2><div class="charts"><div><h3>Area</h3><p class="legend"><span class="opto">Opto</span><span class="yosys">Yosys+ABC</span></p>${{chart(id,'opto_area','yosys_area','opto','yosys')}}</div><div><h3>Worst slack</h3><p class="legend"><span class="opto">Opto</span><span class="yosys">Yosys+ABC</span></p>${{chart(id,'worst_slack','yosys_worst_slack','opto','yosys')}}</div></div><details><summary>Measurements</summary><table><thead><tr><th>Date</th><th>Commit</th><th>Status</th><th>CEC</th><th>Opto area</th><th>Yosys area</th><th>Opto cells</th><th>Yosys cells</th><th>Opto delay</th><th>Yosys delay</th><th>Opto WNS</th><th>Yosys WNS</th><th>Opto TNS</th><th>Yosys TNS</th><th>Opto violating</th><th>Yosys violating</th><th>Opto wall s</th><th>Yosys wall s</th><th>Opto CPU s</th><th>Yosys CPU s</th><th>Opto RSS KiB</th><th>Yosys RSS KiB</th><th>Opto cells by type</th><th>Yosys cells by type</th></tr></thead><tbody>${{rows}}</tbody></table></details></section>`);
}}
</script>
</body>
</html>
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--result", type=Path, required=True)
    parser.add_argument("--history", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--timestamp")
    args = parser.parse_args()

    timestamp = args.timestamp or dt.datetime.now(dt.timezone.utc).isoformat()
    document = json.loads(args.result.read_text(encoding="utf-8"))
    validate_result_document(document)
    history = load_history(args.history)
    if any(point.get("suite") != document["suite"] for point in history["points"]):
        raise ValueError("QoR history contains a different suite")
    history["points"] = [
        point for point in history["points"] if point.get("commit") != args.commit
    ]
    history["points"].append(point_from_result(document, args.commit, timestamp))
    history["points"] = history["points"][-180:]

    args.history.parent.mkdir(parents=True, exist_ok=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.history.write_text(json.dumps(history, indent=2) + "\n", encoding="utf-8")
    args.output.write_text(render(history), encoding="utf-8")


if __name__ == "__main__":
    main()
