package com.example.file.util;

import com.example.file.Store;

public final class Helper {
    private Helper() {
    }

    public static int twice(int value) {
        return value + Store.raw();
    }

    public static int unused() {
        return 0;
    }
}
