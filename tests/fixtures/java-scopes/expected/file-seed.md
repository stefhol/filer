# Code bundle: file-seed

- root: `<root>`
- files: 3
- excerpts: 2
- methods: 2
- import depth: 2

## Files

- `src/main/java/com/example/file/Service.java`
- `src/main/java/com/example/file/Store.java` :: `Store` [raw] (depth 1, call)
- `src/main/java/com/example/file/util/Helper.java` :: `Helper` [twice] (depth 1, call)

## Contents

### `src/main/java/com/example/file/Service.java`

```java
package com.example.file;

import com.example.file.util.Helper;

/**
 * File-seed fixture. Two top-level classes in one file: a file seed must keep
 * both classes whole and follow the calls of both of them at depth 1.
 */
class Service {
    private final Repo repo;

    Service(Repo repo) {
        this.repo = repo;
    }

    int used() {
        return repo.size() + Helper.twice(1);
    }

    int unused() {
        return 0;
    }
}

class Repo {
    int size() {
        return Store.raw();
    }

    int spare() {
        return 0;
    }
}
```

### `src/main/java/com/example/file/Store.java` :: `Store` (line 5)

```java
package com.example.file;

import com.example.file.util.Helper;

public final class Store {
    public static int raw() {
        return Helper.twice(0);
    }

    public static int unused();   // body omitted
}
```

### `src/main/java/com/example/file/util/Helper.java` :: `Helper` (line 5)

```java
package com.example.file.util;

import com.example.file.Store;

public final class Helper {
    private Helper() {
    }

    public static int twice(int value) {
        return value + Store.raw();
    }

    public static int unused();   // body omitted
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
path = "src/main/java/com/example/file/Service.java"
scope = "file"
depth = 0
reason = "seed"

[[bundles.entries]]
path = "src/main/java/com/example/file/Store.java"
scope = "methods"
symbol = "Store"
methods = ["raw"]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/file/util/Helper.java"
scope = "methods"
symbol = "Helper"
methods = ["twice"]
depth = 1
reason = "call"
```
