# Code bundle: fallback

- root: `<root>`
- files: 3
- excerpts: 3
- methods: 1
- import depth: 2

## Files

- `src/main/java/com/example/fallback/Controller.java` :: `Controller`
- `src/main/java/com/example/fallback/Bridge.java` :: `Bridge` [lookup] (depth 1, call)
- `src/main/java/com/example/fallback/Repo.java` :: `Repo` (depth 1, call)

## Contents

### `src/main/java/com/example/fallback/Controller.java` :: `Controller` (line 3)

```java
package com.example.fallback;

/**
 * Fallback fixture. `unknown` and `chained` must stop at the first hop,
 * `missing` must fall back to the whole receiver class, and `constructed`
 * must resolve `new Type().method()`.
 */
public class Controller {

    private final Repo repo;

    public Controller(Repo repo) {
        this.repo = repo;
    }

    public int unknown(Bridge bridge) {
        // lookup() returns Object, so tick() has no known receiver type
        return bridge.lookup().tick();
    }

    public int missing() {
        // Repo is known but has no missing(): keep the whole class
        return repo.missing();
    }

    public int chained() {
        // the first hop resolves, the chained tick() does not
        return new Bridge().lookup().tick();
    }

    public int constructed() {
        // new Type().method() is a supported receiver form
        return new Repo().size();
    }
}
```

### `src/main/java/com/example/fallback/Bridge.java` :: `Bridge` (line 3)

```java
package com.example.fallback;

public class Bridge {
    public Object lookup() {
        return null;
    }

    public Object unused();   // body omitted
}
```

### `src/main/java/com/example/fallback/Repo.java` :: `Repo` (line 3)

```java
package com.example.fallback;

public class Repo {
    public int size() {
        return 1;
    }

    public int unused() {
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
path = "src/main/java/com/example/fallback/Controller.java"
scope = "symbol"
symbol = "Controller"
depth = 0
reason = "seed"

[[bundles.entries]]
path = "src/main/java/com/example/fallback/Bridge.java"
scope = "methods"
symbol = "Bridge"
methods = ["lookup"]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/fallback/Repo.java"
scope = "symbol"
symbol = "Repo"
depth = 1
reason = "call"
```
