# Code bundle: method-seed

- root: `<root>`
- files: 1
- excerpts: 1
- methods: 3
- import depth: 2

## Files

- `src/main/java/com/example/method/Pipeline.java` :: `Pipeline` [prepare, run, toResult]

## Contents

### `src/main/java/com/example/method/Pipeline.java` :: `Pipeline` (line 3)

```java
package com.example.method;

/**
 * Method-seed fixture: intra-class calls and overloads. Overloads resolve by
 * name, so all three `prepare` methods and all three call sites stay together.
 */
public class Pipeline {

    public int run() {
        int first = prepare();
        int second = prepare(1);
        int third = prepare(2, 3);
        return toResult(first + second + third);
    }

    private int prepare() {
        return 0;
    }

    private int prepare(int value) {
        return value;
    }

    private int prepare(int left, int right) {
        return left + right;
    }

    private int toResult(int value) {
        return value;
    }

    private int skipped();   // body omitted
}
```

## Bundle state

```toml
root = "<root>"
version = 3

[[bundles]]
name = "<branch>"
created = <timestamp>
depth = 2

[[bundles.entries]]
path = "src/main/java/com/example/method/Pipeline.java"
scope = "methods"
symbol = "Pipeline"
methods = [
    "prepare",
    "run",
    "toResult",
]
depth = 0
reason = "seed"
```
