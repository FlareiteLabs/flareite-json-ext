; Highlighting, from tree-sitter-json 0.24.8 queries/highlights.scm.
; MIT licensed: see the LICENSE files in ../grammars.

(pair
  key: (_) @string.special.key)

(string) @string

(number) @number

[
  (null)
  (true)
  (false)
] @constant.builtin

(escape_sequence) @escape

(comment) @comment
