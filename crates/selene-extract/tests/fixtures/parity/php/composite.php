<?php

namespace App\Services;

use PHPUnit\Framework\TestCase;
use Mockery as m;

interface Finder
{
    public function find(string $id): ?User;
}

class UserRepository implements Finder
{
    private const TABLE = 'users';

    private Database $db;

    public function __construct(Database $db)
    {
        $this->db = $db;
    }

    public function find(string $id): ?User
    {
        return $this->db->query(self::TABLE, $id);
    }
}

function makeRepository(Database $db): UserRepository
{
    return new UserRepository($db);
}
