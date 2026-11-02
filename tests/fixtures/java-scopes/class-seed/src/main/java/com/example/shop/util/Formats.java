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
