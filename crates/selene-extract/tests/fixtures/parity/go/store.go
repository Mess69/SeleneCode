package store

import (
	"encoding/json"
	"fmt"
)

type Store struct {
	db *Database
}

type Reader interface {
	Read(id string) ([]byte, error)
}

func New(db *Database) *Store {
	return &Store{db: db}
}

func (s *Store) Read(id string) ([]byte, error) {
	raw, err := s.db.Find(id)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", id, err)
	}
	return json.Marshal(raw)
}
