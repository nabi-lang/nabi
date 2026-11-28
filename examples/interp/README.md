# interp

A tiny scripting language written in Nabi: a lexer, a Pratt parser that
builds an AST of enums, and a tree-walking evaluator with lexical scopes
and closures. It shows:

- exhaustive `match` on sum types (`Tok`, `Kind`, `Op`, `Value`);
- `throws (LangError)` / `try` / `catch` for errors that carry a span,
  rendered with `line:col` and a caret under the bad code;
- a `yield fn` lexer, consumed with `try for`;
- `return` as a second thrown type (`throws (LangError | Ret)`);
- `T | none` for lookups and optional names.

## The language

```
program := stmt*
stmt    := "let" NAME "=" expr | "fn" NAME "(" params ")" block
         | "return" expr? | expr                    (each with an optional ";")
expr    := expr OP expr | ("-" | "!") expr | expr "(" args ")" | expr "[" expr "]"
         | NAME "=" expr | expr "[" expr "]" "=" expr
         | NUMBER | STRING | "true" | "false" | "nil" | NAME | "[" args "]"
         | "fn" "(" params ")" block | "if" expr block ("else" (if | block))?
         | "while" expr block | block | "(" expr ")"
block   := "{" stmt* "}"                            (its value is the last stmt's)
```

Binary operators, loosest first: `or`; `and`; `==` `!=`; `<` `<=` `>` `>=`;
`+` `-`; `*` `/` `%`. Assignment is loosest of all and right-associative.

Builtins: `print`, `len`, `push`, `str`, `range`. `#` starts a comment.

## Run

```sh
nabi run -- examples/fib.lang
nabi run -- examples/counter.lang
nabi run -- examples/lists.lang
nabi run                 # REPL on stdin
nabi test
```

```
$ echo 'print(1 + true)' | nabi run
> <stdin>:1:9: error: cannot apply `+` to a number and a bool
  |
1 | print(1 + true)
  |         ^
```
