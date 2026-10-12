#import "lib.typ": *

#show: report.with(title: "Proposed Budget", subtitle: "Fiscal year " + data.target_label)

#text(fill: muted)[
  #data.target_period. Based on fiscal year #data.base_label#if not data.base_complete [, with actuals to #data.actuals_to].
  Budget status: #data.status.
]

#v(4pt)
#figures(data.figures)

#for f in data.funds [
  = #f.title
  #fin-table(
    columns: (2.4fr, 0.95fr, 0.95fr, 1fr, 0.9fr, 1.6fr),
    align: "lrrrrl",
    header: ("Line item", data.base_label + " budget", data.base_label + " actual", data.target_label + " proposed", "Change", "Notes"),
    rows: f.rows,
    size: 9pt,
  )
  #text(9pt, fill: muted, f.note)
]

#if data.grant_note != "" [
  #callout("info", data.grant_note)
]

#if data.notes != "" [
  = Notes
  #data.notes
]

#v(16pt)
#block(breakable: false)[
  = Approval
  Moved that the #data.target_label budget be approved as presented.
  #v(4pt)
  #signatures("Moved by", "Seconded by", "Date approved")
]
