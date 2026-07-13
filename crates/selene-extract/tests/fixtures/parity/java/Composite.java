package com.example.app;

import java.util.List;
import static java.util.Collections.emptyList;

public interface Repository {
    List<String> findAll();
}

class InMemoryRepository implements Repository {
    private final List<String> items;

    InMemoryRepository(List<String> items) {
        this.items = items;
    }

    @Override
    public List<String> findAll() {
        if (items == null) {
            return emptyList();
        }
        return items;
    }
}
