mio-anonymous-pipes
===================

Wrapper around windows anonymous pipes (which are synchronous and do not support IOCP) so that they are asynchronous and implement mio::Evented. Achieves this using a temporary buffer and a worker thread to move data from the synchronous pipe into the buffer.
