// Shared styles for all PDF reports. Each report passes its data as JSON in
// sys.inputs.data. Money arrives pre-formatted in accounting style.

#let data = json(bytes(sys.inputs.data))

#let accent = rgb("#1d4f73")
#let muted = rgb("#5f6b76")
#let rule = rgb("#c9d1d9")
#let soft = rgb("#eef3f7")
#let bad = rgb("#a4262c")
#let warn = rgb("#8a5a00")

#let report(title: "", subtitle: "", body) = {
  set document(title: title + " — " + data.org, author: data.prepared_by, keywords: ("PAC Keeper",))
  set page(
    paper: "us-letter",
    margin: (x: 0.7in, top: 0.85in, bottom: 0.8in),
    header: context {
      if counter(page).get().first() > 1 {
        set text(8pt, fill: muted)
        grid(columns: (1fr, auto), data.org, title + " · " + subtitle)
        v(-4pt)
        line(length: 100%, stroke: 0.4pt + rule)
      }
    },
    footer: context {
      set text(8pt, fill: muted)
      grid(
        columns: (1fr, auto),
        [Prepared #data.prepared_on#if data.prepared_by != "" [ by #data.prepared_by] with PAC Keeper],
        counter(page).display("1 of 1", both: true),
      )
    },
  )
  set text(font: "Libertinus Serif", size: 10pt, number-type: "lining", number-width: "tabular")
  set par(justify: false, leading: 0.55em)
  show heading.where(level: 1): it => {
    v(10pt)
    block(text(13pt, weight: "bold", fill: accent, it.body))
    v(2pt)
  }
  show heading.where(level: 2): it => {
    v(6pt)
    block(text(11pt, weight: "semibold", it.body))
  }

  // Title block
  block(width: 100%, inset: (bottom: 8pt), stroke: (bottom: 1.2pt + accent))[
    #text(10pt, fill: muted, data.org) \
    #v(2pt)
    #text(20pt, weight: "bold", fill: accent, title) \
    #v(1pt)
    #text(12pt, subtitle)
  ]
  v(4pt)
  body
}

// A financial table. rows is an array of dictionaries:
//   (kind: "row" | "section" | "subtotal" | "total" | "note", cells: (...))
// align is a string with one letter per column: "l" left, "r" right, "c" centre.
#let fin-table(columns: (), align: "", header: (), rows: (), size: 9.5pt) = {
  let n = columns.len()
  let al = align.clusters().map(a => if a == "r" { right } else if a == "c" { center } else { left })
  set text(size)
  // Keep short tables on one page; let long ones flow.
  block(breakable: rows.len() > 28, table(
    columns: columns,
    align: (x, y) => al.at(x, default: left) + horizon,
    stroke: none,
    inset: (x: 5pt, y: 3.6pt),
    table.header(
      ..header.map(h => table.cell(fill: soft, text(8.5pt, weight: "bold", fill: accent, h))),
    ),
    table.hline(stroke: 0.7pt + accent),
    ..rows
      .map(r => {
        if r.kind == "section" {
          (table.cell(colspan: n, inset: (top: 7pt, bottom: 3pt, x: 5pt), text(weight: "bold", r.cells.at(0))),)
        } else if r.kind == "note" {
          (table.cell(colspan: n, text(8.5pt, style: "italic", fill: muted, r.cells.at(0))),)
        } else if r.kind == "subtotal" {
          (table.hline(stroke: 0.4pt + rule), ..r.cells.map(c => text(weight: "semibold", c)))
        } else if r.kind == "total" {
          (table.hline(stroke: 0.9pt + black), ..r.cells.map(c => text(weight: "bold", c)))
        } else if r.kind == "flag" {
          r.cells.map(c => text(fill: bad, c))
        } else {
          r.cells.map(c => [#c])
        }
      })
      .flatten(),
    table.hline(stroke: 0.4pt + rule),
  ))
}

// Key figures in a row of boxes.
#let figures(items) = {
  grid(
    columns: (1fr,) * items.len(),
    column-gutter: 8pt,
    ..items.map(it => block(
      width: 100%,
      fill: soft,
      radius: 3pt,
      inset: 9pt,
      [
        #text(8pt, fill: muted, upper(it.label)) \
        #v(1pt)
        #text(14pt, weight: "bold", it.value)
        #if it.at("note", default: "") != "" [ \ #text(8pt, fill: muted, it.note)]
      ],
    ))
  )
}

#let callout(level, body) = {
  let color = if level == "problem" { bad } else if level == "warning" { warn } else { muted }
  block(
    width: 100%,
    inset: (left: 8pt, y: 4pt, right: 4pt),
    stroke: (left: 2pt + color),
    text(9pt, fill: color, body),
  )
}

#let signature(label) = box(width: 100%)[
  #v(26pt)
  #line(length: 100%, stroke: 0.5pt)
  #text(8.5pt, fill: muted, label)
]

#let signatures(..labels) = grid(
  columns: (1fr,) * labels.pos().len(),
  column-gutter: 18pt,
  ..labels.pos().map(signature),
)
