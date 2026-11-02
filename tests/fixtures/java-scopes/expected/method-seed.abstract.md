# Code bundle: method-seed

- root: `<root>`
- files: 1
- excerpts: 1
- methods: 3
- import depth: 2

## Files

- `src/main/java/com/example/method/base/Job.java` :: `Job` [labels, runOnce, step]

## Contents

### `src/main/java/com/example/method/base/Job.java` :: `Job` (line 3)

```java
package com.example.method.base;

/**
 * Abstract class seeded through a concrete method: the abstract callee has no
 * body, so it is kept as a signature while the concrete sibling stays whole.
 */
public abstract class Job {

    public int runOnce() {
        return step() + labels();
    }

    protected abstract int step();

    protected int labels() {
        return 0;
    }
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
path = "src/main/java/com/example/method/base/Job.java"
scope = "methods"
symbol = "Job"
methods = [
    "labels",
    "runOnce",
    "step",
]
depth = 0
reason = "seed"
```
