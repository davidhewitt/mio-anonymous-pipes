mio-anonymous-pipes
===================

Wrapper around windows anonymous pipes (which are synchronous and do not support IOCP) so that they are asynchronous and implement mio::Evented. Achieves this using a temporary buffer and a worker thread to move data from the synchronous pipe into the buffer.

This might all be removable in favour of a standard tool in mio.

Alternatively when the Conpty API supports overlapped IO (https://github.com/Microsoft/console/issues/262) then mio_named_pipes could be used instead of this.

TODO (Pending knowing whether such a standard tool exists):
 - Proper drop implementations on all the types
 - Error handling if the worker threads encounter read / write errors
 - Performance testing
