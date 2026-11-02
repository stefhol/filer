# Code bundle: class-seed

- root: `<root>`
- files: 6
- excerpts: 6
- methods: 7
- import depth: 2

## Files

- `src/main/java/com/example/shop/Service.java` :: `Service`
- `src/main/java/com/example/shop/Cart.java` :: `Cart` [cartCount] (depth 1, call)
- `src/main/java/com/example/shop/Ledger.java` :: `Ledger` [ledgerTotal] (depth 1, call)
- `src/main/java/com/example/shop/Repo.java` :: `Repo` [repoSize] (depth 1, call)
- `src/main/java/com/example/shop/base/BaseService.java` :: `BaseService` [audit, base] (depth 1, call)
- `src/main/java/com/example/shop/util/Formats.java` :: `Formats` [money, zero] (depth 1, call)

## Contents

### `src/main/java/com/example/shop/Service.java` :: `Service` (line 8)

```java
package com.example.shop;

import com.example.shop.base.BaseService;
import com.example.shop.util.Formats;

import static com.example.shop.util.Formats.money;

/**
 * Class-seed fixture. Every receiver kind of the plan appears exactly once:
 * field, parameter, local, static type, static import, inherited bare call and
 * inherited call through `super`.
 */
public class Service extends BaseService {
    private final Repo repo = new Repo();

    public int total(Ledger param) {
        Cart local = new Cart();
        int a = repo.repoSize();     // field
        int b = param.ledgerTotal(); // parameter
        int c = local.cartCount();   // local
        int d = Formats.zero();      // static type
        int e = base();              // inherited, bare
        return a + b + c + d + e;
    }

    public void inspect() {
        audit();      // inherited, bare
        super.base(); // inherited, super
    }

    public String label() {
        return money(7); // static import
    }
}
```

### `src/main/java/com/example/shop/Cart.java` :: `Cart` (line 3)

```java
package com.example.shop;

public class Cart {
    public int cartCount() {
        return 1;
    }

    public int cartUnused();   // body omitted
}
```

### `src/main/java/com/example/shop/Ledger.java` :: `Ledger` (line 3)

```java
package com.example.shop;

public class Ledger {
    public int ledgerTotal() {
        return 1;
    }

    public int ledgerUnused();   // body omitted
}
```

### `src/main/java/com/example/shop/Repo.java` :: `Repo` (line 3)

```java
package com.example.shop;

public class Repo {
    public int repoSize() {
        return 1;
    }

    public int repoUnused();   // body omitted
}
```

### `src/main/java/com/example/shop/base/BaseService.java` :: `BaseService` (line 3)

```java
package com.example.shop.base;

public abstract class BaseService {
    public int base() {
        return 1;
    }

    protected void audit() {
    }
}
```

### `src/main/java/com/example/shop/util/Formats.java` :: `Formats` (line 3)

```java
package com.example.shop.util;

public final class Formats {
    private Formats() {
    }

    public static String money(int value) {
        return "$" + value;
    }

    public static int zero() {
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
path = "src/main/java/com/example/shop/Service.java"
scope = "symbol"
symbol = "Service"
depth = 0
reason = "seed"

[[bundles.entries]]
path = "src/main/java/com/example/shop/Cart.java"
scope = "methods"
symbol = "Cart"
methods = ["cartCount"]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/shop/Ledger.java"
scope = "methods"
symbol = "Ledger"
methods = ["ledgerTotal"]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/shop/Repo.java"
scope = "methods"
symbol = "Repo"
methods = ["repoSize"]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/shop/base/BaseService.java"
scope = "methods"
symbol = "BaseService"
methods = [
    "audit",
    "base",
]
depth = 1
reason = "call"

[[bundles.entries]]
path = "src/main/java/com/example/shop/util/Formats.java"
scope = "methods"
symbol = "Formats"
methods = [
    "money",
    "zero",
]
depth = 1
reason = "call"
```
