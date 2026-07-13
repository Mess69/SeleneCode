interface Greeter {
    fun greet(): String
}

open class BaseService

class UserGreeter(private val name: String) : BaseService(), Greeter {
    override fun greet(): String {
        return name
    }
}
