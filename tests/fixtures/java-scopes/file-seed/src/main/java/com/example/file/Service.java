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
