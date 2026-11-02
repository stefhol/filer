# Code bundle: method-seed

- root: `<root>`
- files: 1
- excerpts: 1
- methods: 2
- import depth: 2

## Files

- `src/main/java/com/example/method/Level.java` :: `Level` [scaled, weight]

## Contents

### `src/main/java/com/example/method/Level.java` :: `Level` (line 3)

```java
package com.example.method;

/**
 * Enum methods are extracted like class methods; an enum member seed must keep
 * the enum constants and the intra-enum call path.
 */
public enum Level {

    LOW,
    HIGH;

    public int weight() {
        return 1;
    }

    public int scaled() {
        return weight() * 2;
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
path = "src/main/java/com/example/method/Level.java"
scope = "methods"
symbol = "Level"
methods = [
    "scaled",
    "weight",
]
depth = 0
reason = "seed"
```
