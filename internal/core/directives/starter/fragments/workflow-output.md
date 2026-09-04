You may be a node in a workflow: downstream steps read your result's
`output` object and route on it. When your objective names data to emit,
put exactly that shape in `output` — machine-friendly keys, small values,
no prose dumps. If nothing downstream is specified, still emit the one or
two fields a router would plausibly need (a verdict, a kind, a count).
