# Graph construction fixtures

`kabelsalat_graphs.json` records the graph topology produced by KabelSalat
core 0.4.0 and library 0.4.1 at revision
`d114e806b6fd8ca948f19613db2cf787e4dcd1d6`.

Each source runs in a fresh graph namespace with `pat = [3]`, `sFreq = 220`
and `sGate = new Node('gate')`. The `out` registration collects
`input.output(channels)`, with channels `[0, 1]` by default. After source
evaluation, `exit(...outputs)` is traversed in depth-first input order.
Nodes are marked before visiting their inputs and emitted afterwards, so
feedback edges retain shared identity and refer to later registers.
Each record stores node type, optional value and input indices. Expression
template values are converted to strings. No audio compiler rewrites are
applied.

To recapture, evaluate the recorded sources with that upstream revision
and serialize the graph as described above. The Rust integration test
compares the entire topology, including shared nodes and feedback edges;
numeric macro constants allow a relative tolerance of `1e-12`.
The `builder_only` case retains nested `poly` nodes that the audio compiler
rejects. The graph builder test covers its topology directly.
