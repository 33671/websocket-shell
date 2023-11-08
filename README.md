A websocket based arbitrary shell command executor with long-keeping and reusable input and output.
## Run
```shell
./websocket-shell 0.0.0.0:8080
```
### Example
```shell
wscat ws://127.0.0.1:8080
Connected (press CTRL+C to quit)
> uname
< Linux

> whoami
< zhang

> 
```
send string `ctrl+c` to exit running shell child process