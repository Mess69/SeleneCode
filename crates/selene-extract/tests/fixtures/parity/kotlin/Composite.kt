import java.io.IOException
import okhttp3.Request.Builder as RequestBuilder

interface Fetcher {
    fun fetch(url: String): String
}

class HttpFetcher(private val client: Client) : Fetcher {
    override fun fetch(url: String): String {
        val request = RequestBuilder().url(url).build()
        return client.execute(request)
    }
}

fun main() {
    val fetcher = HttpFetcher(Client())
    fetcher.fetch("https://example.com")
}
