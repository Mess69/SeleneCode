using System;
using System.Collections.Generic;

namespace App.Services;

public interface IUserRepository
{
    User FindById(string id);
}

public class UserService : IUserRepository
{
    private readonly IUserRepository _repository;

    public UserService(IUserRepository repository)
    {
        _repository = repository;
    }

    public User FindById(string id)
    {
        Console.WriteLine(id);
        return _repository.FindById(id);
    }
}
