//
// zhtta.rs
//
// Starting code for PS3
// Running on Rust 0.9
//
// Note that this code has serious security risks!  You should not run it 
// on any system with access to sensitive files.
// 
// University of Virginia - cs4414 Spring 2014
// Weilin Xu and David Evans
// Version 0.5

// To see debug! outputs set the RUST_LOG environment variable, e.g.: export RUST_LOG="zhtta=debug"

#[feature(globs)];
extern mod extra;

use std::io::*;
use std::io::fs::stat;
use std::io::net::ip::{SocketAddr};
use std::{os, str, libc, from_str};
use std::path::Path;
use std::hashmap::HashMap;

use extra::getopts;
use extra::arc::MutexArc;
use extra::arc::RWArc;
use extra::sync::Semaphore;

mod gash;

static SERVER_NAME : &'static str = "Zhtta Version 0.5";

static IP : &'static str = "127.0.0.1";
static PORT : uint = 4414;
static WWW_DIR : &'static str = "./www";
static TASKS : int = 8;
static CACHE : uint = 2;

static HTTP_OK : &'static str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n";
static HTTP_BAD : &'static str = "HTTP/1.1 404 Not Found\r\n\r\n";


static COUNTER_STYLE : &'static str = "<doctype !html><html><head><title>Hello, Rust!</title>
             <style>body { background-color: #884414; color: #FFEEAA}
                    h1 { font-size:2cm; text-align: center; color: black; text-shadow: 0 0 4mm red }
                    h2 { font-size:2cm; text-align: center; color: black; text-shadow: 0 0 4mm green }
             </style></head>
             <body>";

struct HTTP_Request {
    // Use peer_name as the key to access TcpStream in hashmap. 

    // (Due to a bug in extra::arc in Rust 0.9, it is very inconvenient to use TcpStream without the "Freeze" bound.
    //  See issue: https://github.com/mozilla/rust/issues/12139)
    peer_name: ~str,
    path: ~Path,
    size: u64,
}

struct WebServer {
    ip: ~str,
    port: uint,
    www_dir_path: ~Path,
    
    request_queue_arc: MutexArc<~[HTTP_Request]>,
    stream_map_arc: MutexArc<HashMap<~str, Option<std::io::net::tcp::TcpStream>>>,
    
    notify_port: Port<()>,
    shared_notify_chan: SharedChan<()>,

    visitor_count: RWArc<uint>,

    req_handling_tasks: Semaphore,

    cached_pages: MutexArc<HashMap<Path, ~[u8]>>,

    cached_threshold: uint,
}

impl WebServer {
    fn new(ip: &str, port: uint, www_dir: &str, tasks: int, cache: uint) -> WebServer {
        let (notify_port, shared_notify_chan) = SharedChan::new();
        let www_dir_path = ~Path::new(www_dir);
        os::change_dir(www_dir_path.clone());
       
        WebServer {
            ip: ip.to_owned(),
            port: port,
            www_dir_path: www_dir_path,
                        
            request_queue_arc: MutexArc::new(~[]),
            stream_map_arc: MutexArc::new(HashMap::new()),
            
            notify_port: notify_port,
            shared_notify_chan: shared_notify_chan,
            
            visitor_count: RWArc::new(0),

            req_handling_tasks: Semaphore::new(tasks),

            cached_pages: MutexArc::new(HashMap::new()),

            cached_threshold: cache * 1000000,
        }
    }
    
    fn run(&mut self) {
        self.listen();
        self.dequeue_static_file_request();
    }
    
    fn listen(&mut self) {
        let addr = from_str::<SocketAddr>(format!("{:s}:{:u}", self.ip, self.port)).expect("Address error.");
        let www_dir_path_str = self.www_dir_path.as_str().expect("invalid www path?").to_owned();
        
        let request_queue_arc = self.request_queue_arc.clone();
        let shared_notify_chan = self.shared_notify_chan.clone();
        let stream_map_arc = self.stream_map_arc.clone();
       
        let (visitor_port, visitor_chan) = Chan::new();
        visitor_chan.send(self.visitor_count.clone());
        
        let (cached_pages_port, cached_pages_chan) = Chan::new();
        cached_pages_chan.send(self.cached_pages.clone());

        let (semaphore_port, semaphore_chan) = Chan::new();
        semaphore_chan.send(self.req_handling_tasks.clone());

        spawn(proc() {
            let mut acceptor = net::tcp::TcpListener::bind(addr).listen();
            println!("{:s} listening on {:s} (serving from: {:s}).", 
                     SERVER_NAME, addr.to_str(), www_dir_path_str);
            
            let local_copy = visitor_port.recv();
            let local_cached_pages = cached_pages_port.recv();
            let local_semaphore = semaphore_port.recv();
    
            for stream in acceptor.incoming() {
                let (queue_port, queue_chan) = Chan::new();
                queue_chan.send(request_queue_arc.clone());
                
                let notify_chan = shared_notify_chan.clone();
                let stream_map_arc = stream_map_arc.clone();

                let (next_visitor_port, next_visitor_chan) = Chan::new();
                next_visitor_chan.send(local_copy.clone());
                
                let (next_cache_map_port, next_cache_map_chan) = Chan::new();
                next_cache_map_chan.send(local_cached_pages.clone());

                let (next_sem_port, next_sem_chan) = Chan::new();
                next_sem_chan.send(local_semaphore.clone());

                // Spawn a task to handle the connection.
                spawn(proc() {
                    let another_local_copy = next_visitor_port.recv();
                    another_local_copy.write(|number| { *number += 1 });
                    let request_queue_arc = queue_port.recv();
                  
                    let mut stream = stream;
                    
                    let peer_name = WebServer::get_peer_name(&mut stream);
                            
                    let mut buf = [0, ..800];
                    stream.read(buf);
                   
                    let request_str = str::from_utf8(buf);
                    debug!("Request:\n{:s}", request_str);
                    
                    let req_group : ~[&str]= request_str.splitn(' ', 3).collect();
                    if req_group.len() > 2 {
                        let path_str = "." + req_group[1].to_owned();
                        
                        let mut path_obj = ~os::getcwd();
                        path_obj.push(path_str.clone());
                        
                        let ext_str = match path_obj.extension_str() {
                            Some(e) => e,
                            None => "",
                        };
                        
                        debug!("Requested path: [{:s}]", path_obj.as_str().expect("error"));
                        debug!("Requested path: [{:s}]", path_str);
                             
                        if path_str == ~"./" {
                            debug!("===== Counter Page request =====");
                            let visitorno = another_local_copy.read(|number| { *number });
                            WebServer::respond_with_counter_page(stream, visitorno);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else if !path_obj.exists() || path_obj.is_dir() {
                            debug!("===== Error page request =====");
                            WebServer::respond_with_error_page(stream, path_obj);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else if ext_str == "shtml" { // Dynamic web pages.
                            debug!("===== Dynamic Page request =====");
                            WebServer::respond_with_dynamic_page(stream, path_obj);
                            debug!("=====Terminated connection from [{:s}].=====", peer_name);
                        } else { 
                            debug!("===== Static Page request =====");
                            let local_sem = next_sem_port.recv();
                            let local_map_copy = next_cache_map_port.recv();
                            let mapcontains = local_map_copy.access(|local_cached_map| {
                                local_cached_map.contains_key(path_obj)
                            });
                            if mapcontains {
                                 local_sem.acquire();
                                 let (send_sem_port, send_sem_chan) = Chan::new();
                                 send_sem_chan.send(local_sem.clone());
                                 let (send_stream_port, send_stream_chan) = Chan::new();
                                 send_stream_chan.send(stream);
                                 let (send_path_port, send_path_chan) = Chan::new();
                                 send_path_chan.send(path_obj.clone());
                                 spawn(proc() {
                                      let this_sem = send_sem_port.recv();
                                      let mut this_stream = send_stream_port.recv();
                                      let this_path = send_path_port.recv();
                                      this_sem.access(|| {
                                          local_map_copy.access(|local_cached_map| {
                                              this_stream.write(HTTP_OK.as_bytes());
                                              this_stream.write(*local_cached_map.find(this_path).unwrap());
                                          });
                                      });
                                      this_sem.release();
                                  });              
                            }
                            else {
                                 WebServer::enqueue_static_file_request(stream, path_obj, stream_map_arc, request_queue_arc, notify_chan);
                            }
                        }
                    }
                });
            }
        });
    }

    fn respond_with_error_page(stream: Option<std::io::net::tcp::TcpStream>, path: &Path) {
        let mut stream = stream;
        let msg: ~str = format!("Cannot open: {:s}", path.as_str().expect("invalid path").to_owned());

        stream.write(HTTP_BAD.as_bytes());
        stream.write(msg.as_bytes());
    }

    fn respond_with_counter_page(stream: Option<std::io::net::tcp::TcpStream>, visitorcount: uint) {
        let mut stream = stream;
        let response: ~str = 
            format!("{:s}{:s}<h1>Greetings, Krusty!</h1>
                     <h2>Visitor count: {:u}</h2></body></html>\r\n", 
                    HTTP_OK, COUNTER_STYLE, 
                    visitorcount);
        debug!("Responding to counter request");
        stream.write(response.as_bytes());
    }
    
    fn respond_with_static_file(stream: Option<std::io::net::tcp::TcpStream>, path: &Path, cached_pages: MutexArc<HashMap<Path, ~[u8]>>, cache_thresh: uint) {
        let mut stream = stream;
        let mapcontains = cached_pages.access(|local_cached_map| {
            local_cached_map.contains_key(path)
        });
        if mapcontains {
            cached_pages.access(|local_cached_map| {
                stream.write(HTTP_OK.as_bytes());
                stream.write(*local_cached_map.find(path).unwrap());
            });
            
        }
        else {
            let mut file_reader = File::open(path).expect("Invalid file!");
            let file_size : u64 = std::io::fs::stat(path).size;
            let size_in_byte : uint = file_size.to_uint().unwrap();
            let mut output_page : ~[u8] = ~[];
           
            stream.write(HTTP_OK.as_bytes());
            let mut size_remaining = size_in_byte;
            while size_remaining > 256 {
                let write_bytes = file_reader.read_bytes(256);
                output_page = std::vec::append(output_page, write_bytes);
                stream.write(write_bytes);
                size_remaining -= 256;
            }
            let write_bytes = file_reader.read_bytes(size_remaining);
            output_page = std::vec::append(output_page, write_bytes);
            stream.write(write_bytes);
            
            if size_in_byte <= cache_thresh {
                println!("caching {:u}", size_in_byte);
                cached_pages.access(|local_cached_map| {
                    local_cached_map.insert(path.clone(), output_page.clone());
                }); 
            }
        }
    }
    
    fn respond_with_dynamic_page(stream: Option<std::io::net::tcp::TcpStream>, path: &Path) {
        let mut stream = stream;
        let mut file_reader = File::open(path).expect("Invalid file!");
        let original_html = file_reader.read_to_str();
        let startindex = original_html.find_str("<!--#exec cmd=\"").unwrap();
        let endindex = original_html.find_str("\" -->").unwrap();
        let cmd = original_html.slice(startindex + 15, endindex);
        let run_result = gash::run_cmdline(cmd);
        let mut output_html = original_html.slice_to(startindex).to_owned();
        output_html = output_html.append(run_result);
        output_html = output_html.append(original_html.slice_from(endindex + 5).to_owned());
        stream.write(HTTP_OK.as_bytes());
        stream.write(output_html.as_bytes());
    }
    
    fn enqueue_static_file_request(stream: Option<std::io::net::tcp::TcpStream>, path_obj: &Path, stream_map_arc: MutexArc<HashMap<~str, Option<std::io::net::tcp::TcpStream>>>, req_queue_arc: MutexArc<~[HTTP_Request]>, notify_chan: SharedChan<()>) {
        // Save stream in hashmap for later response.
        let mut stream = stream;
        let peer_name = WebServer::get_peer_name(&mut stream);
        let (stream_port, stream_chan) = Chan::new();
        stream_chan.send(stream);
        unsafe {
            // Use an unsafe method, because TcpStream in Rust 0.9 doesn't have "Freeze" bound.
            stream_map_arc.unsafe_access(|local_stream_map| {
                let stream = stream_port.recv();
                local_stream_map.swap(peer_name.clone(), stream);
            });
        }
        // Enqueue the HTTP request.
        let req = HTTP_Request { peer_name: peer_name.clone(), path: ~path_obj.clone(), size: std::io::fs::stat(path_obj).size };
        let (req_port, req_chan) = Chan::new();
        req_chan.send(req);

        debug!("Waiting for queue mutex lock.");
        req_queue_arc.access(|local_req_queue| {
            debug!("Got queue mutex lock.");
            let req: HTTP_Request = req_port.recv();
            let add_ip = req.peer_name.clone();
            let current_start_ip: ~[&str] = add_ip.split('.').collect();
            let priority = match (current_start_ip[0], current_start_ip[1]) {
                    ("128", "143") => true,
                    ("137", "54") => true,
                    _ => false,
            };
            let mut insert_index = local_req_queue.len();
            if priority {
                    for i in range(0, local_req_queue.len()) {
                        let this_ip : ~str = local_req_queue[i].peer_name.clone();
                        let ip_vec : ~[&str] = this_ip.split('.').collect();
                        match (ip_vec[0], ip_vec[1]) {
                            ("128", "143") => insert_index = i+1,
                            ("137", "54") => insert_index = i+1,
                            _ => break,
                        }
                    };
            }
            while insert_index != 0 {
                let prev_file_size = local_req_queue[insert_index - 1].size;
                if req.size < prev_file_size {
                    insert_index -= 1;
                }
                else {
                    break;
                }
            }
            local_req_queue.insert(insert_index, req);
            debug!("A new request enqueued, now the length of queue is {:u}.", local_req_queue.len());
        });
        
        notify_chan.send(()); // Send incoming notification to responder task.
     
    }
    
    fn dequeue_static_file_request(&mut self) {
        let req_queue_get = self.request_queue_arc.clone();
        let stream_map_get = self.stream_map_arc.clone();
        
        // Port<> cannot be sent to another task. So we have to make this task as the main task that can access self.notify_port.
        
        let (request_port, request_chan) = Chan::new();
        loop {
            self.notify_port.recv();    // waiting for new request enqueued.
            
            req_queue_get.access( |req_queue| {
                match req_queue.shift_opt() { // FIFO queue.
                    None => { /* do nothing */ }
                    Some(req) => {
                        request_chan.send(req);
                        debug!("A new request dequeued, now the length of queue is {:u}.", req_queue.len());
                    }
                }
            });
            
            let request = request_port.recv();
            
            // Get stream from hashmap.
            // Use unsafe method, because TcpStream in Rust 0.9 doesn't have "Freeze" bound.
            let (stream_port, stream_chan) = Chan::new();
            unsafe {
                stream_map_get.unsafe_access(|local_stream_map| {
                    let stream = local_stream_map.pop(&request.peer_name).expect("no option tcpstream");
                    stream_chan.send(stream);
                });
            }
            
            self.req_handling_tasks.acquire();
            let (semaphore_port, semaphore_chan) = Chan::new();
            let (cached_port, cached_chan) = Chan::new();
            let (thresh_port, thresh_chan) = Chan::new();
            let mutable_map = self.cached_pages.clone();
            cached_chan.send(mutable_map);
            thresh_chan.send(self.cached_threshold.clone());
            semaphore_chan.send(self.req_handling_tasks.clone());
            spawn(proc() {
                let req_tasks = semaphore_port.recv();
                req_tasks.access(|| {        
                    let stream = stream_port.recv();
                    let cached_map = cached_port.recv();
                    let cache_thresh = thresh_port.recv();
                    WebServer::respond_with_static_file(stream, request.path, cached_map, cache_thresh);
                });
                req_tasks.release();
                debug!("=====Terminated connection from [{:s}].=====", request.peer_name);
            });
        }
    }
    
    fn get_peer_name(stream: &mut Option<std::io::net::tcp::TcpStream>) -> ~str {
        match *stream {
            Some(ref mut s) => {
                         match s.peer_name() {
                            Some(pn) => {pn.to_str()},
                            None => (~"")
                         }
                       },
            None => (~"")
        }
    }
}

fn get_args() -> (~str, uint, ~str, int, uint) {
    fn print_usage(program: &str) {
        println!("Usage: {:s} [options]", program);
        println!("--ip     \tIP address, \"{:s}\" by default.", IP);
        println!("--port   \tport number, \"{:u}\" by default.", PORT);
        println!("--www    \tworking directory, \"{:s}\" by default.", WWW_DIR);
        println!("--task   \tnumber of tasks for handling requests, \"{:d}\" by default.", TASKS);
        println!("--cache  \tsize of cache in MB for pages, \"{:u}\" by default.", CACHE);
        println("-h --help \tUsage");
    }
    
    /* Begin processing program arguments and initiate the parameters. */
    let args = os::args();
    let program = args[0].clone();
    
    let opts = ~[
        getopts::optopt("ip"),
        getopts::optopt("port"),
        getopts::optopt("www"),
        getopts::optopt("task"),
        getopts::optopt("cache"),
        getopts::optflag("h"),
        getopts::optflag("help")
    ];

    let matches = match getopts::getopts(args.tail(), opts) {
        Ok(m) => { m }
        Err(f) => { fail!(f.to_err_msg()) }
    };

    if matches.opt_present("h") || matches.opt_present("help") {
        print_usage(program);
        unsafe { libc::exit(1); }
    }
    
    let ip_str = if matches.opt_present("ip") {
                    matches.opt_str("ip").expect("invalid ip address?").to_owned()
                 } else {
                    IP.to_owned()
                 };
    
    let port:uint = if matches.opt_present("port") {
                        from_str::from_str(matches.opt_str("port").expect("invalid port number?")).expect("not uint?")
                    } else {
                        PORT
                    };
    
    let www_dir_str = if matches.opt_present("www") {
                        matches.opt_str("www").expect("invalid www argument?") 
                      } else { WWW_DIR.to_owned() };

    let tasks:int = if matches.opt_present("task") {
                         from_str::from_str(matches.opt_str("task").expect("invalid task number?")).expect("not uint?")
                     } else {
                        TASKS
                     };
    
    let cache:uint = if matches.opt_present("cache") {
                         from_str::from_str(matches.opt_str("cache").expect("invalid cache size?")).expect("not uint?")
                    } else {
                       CACHE
                    };
    
    (ip_str, port, www_dir_str, tasks, cache)
}

fn main() {
    let (ip_str, port, www_dir_str, tasks, cache) = get_args();
    let mut zhtta = WebServer::new(ip_str, port, www_dir_str, tasks, cache);
    zhtta.run();
}
