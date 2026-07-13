
public class Reader
{
    private enum ReadType
    {
#if HAVE_DATE_TIME_OFFSET
        ReadAsDateTimeOffset,
#endif
        ReadAsDouble,
        ReadAsString,
    }

    public void Open() { }
    public void Close() { }
    public int ReadInt() { return 0; }
}
