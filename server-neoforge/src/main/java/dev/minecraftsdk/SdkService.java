package dev.minecraftsdk;

import com.google.gson.*;
import com.mojang.authlib.GameProfile;
import com.sun.net.httpserver.*;
import dev.minecraftsdk.mixin.NetworkRegistryAccessor;
import net.minecraft.network.Connection;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.level.GameType;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.nio.file.attribute.PosixFilePermissions;
import java.security.*;
import java.util.*;
import java.util.concurrent.*;

/** Trusted private control plane: admission and observations only, never gameplay mutations. */
public final class SdkService implements AutoCloseable {
    public static volatile SdkService active;
    static final Gson JSON=new Gson();
    private final MinecraftServer server;
    private final HttpServer http;
    private final ThreadPoolExecutor executor;
    private final byte[] token;
    private final Path registryPath;
    private final Map<UUID,String> identities=new HashMap<>();
    private final Map<String,java.util.function.BiFunction<ServerPlayer,JsonObject,Object>> observationProviders=new TreeMap<>();
    /** Installed server code only. Requests cannot install providers or select classes. */
    public synchronized void registerObservationProvider(String capability,java.util.function.BiFunction<ServerPlayer,JsonObject,Object> provider){
        if(!capability.matches("[a-z0-9_.-]+:[a-z0-9_./-]+")||capability.length()>128||provider==null||observationProviders.putIfAbsent(capability,provider)!=null)
            throw new IllegalArgumentException("Unique namespaced observation provider required");
    }
    private final Map<Integer,Ticket> tickets=new HashMap<>();
    private final Map<Connection,UUID> admitted=Collections.synchronizedMap(new WeakHashMap<>());
    private record Ticket(UUID uuid,String name,long expires){}
    public SdkService(MinecraftServer server)throws Exception{
        this.server=server;
        if(!server.usesAuthentication())throw new IllegalStateException("SDK requires online-mode=true");
        Path directory=Path.of("minecraft-sdk");
        if(Files.isSymbolicLink(directory))throw new IllegalStateException("SDK directory cannot be a symlink");
        Files.createDirectories(directory,PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")));
        if(!Files.getPosixFilePermissions(directory).equals(PosixFilePermissions.fromString("rwx------")))throw new IllegalStateException("SDK directory requires mode 700");
        var secret=new byte[32];new SecureRandom().nextBytes(secret);
        token=HexFormat.of().formatHex(secret).getBytes(StandardCharsets.US_ASCII);
        Path tokenPath=directory.resolve("native.token");
        if(Files.isSymbolicLink(tokenPath))throw new IllegalStateException("SDK token cannot be a symlink");
        Files.deleteIfExists(tokenPath);
        Files.write(tokenPath,token,StandardOpenOption.CREATE_NEW,StandardOpenOption.WRITE);
        Files.setPosixFilePermissions(tokenPath,PosixFilePermissions.fromString("rw-------"));
        registryPath=directory.resolve("identities.json");
        if(Files.exists(registryPath)){
            if(Files.isSymbolicLink(registryPath)||Files.size(registryPath)>16*1024*1024)throw new IllegalStateException("Invalid SDK registry");
            JsonObject object=JsonParser.parseString(Files.readString(registryPath)).getAsJsonObject();
            for(var entry:object.entrySet()){UUID id=UUID.fromString(entry.getKey());String name=entry.getValue().getAsString();validateIdentity(id,name);if(identities.containsValue(name))throw new IllegalStateException("Duplicate SDK name");identities.put(id,name);}
        }
        int port=Integer.getInteger("minecraftsdk.port",19135);
        http=HttpServer.create(new InetSocketAddress(InetAddress.getByName("127.0.0.1"),port),16);
        executor=new ThreadPoolExecutor(2,2,0,TimeUnit.SECONDS,new ArrayBlockingQueue<>(32),new ThreadPoolExecutor.AbortPolicy());
        http.setExecutor(executor);
        http.createContext("/v1/",this::handle);http.start();
    }
    private void handle(HttpExchange exchange)throws java.io.IOException{
        try(exchange){
          try{
            String auth=exchange.getRequestHeaders().getFirst("Authorization");
            if(auth==null||!auth.startsWith("Bearer ")||!MessageDigest.isEqual(token,auth.substring(7).getBytes(StandardCharsets.US_ASCII))){reply(exchange,401,Map.of("error","unauthorized"));return;}
            if(!exchange.getRequestMethod().equals("POST")||exchange.getRequestURI().getRawQuery()!=null){reply(exchange,405,Map.of("error","method"));return;}
            byte[] bytes=exchange.getRequestBody().readNBytes(16385);if(bytes.length>16384){reply(exchange,413,Map.of("error","request_too_large"));return;}
            JsonObject request=JsonParser.parseString(new String(bytes,StandardCharsets.UTF_8)).getAsJsonObject();
            String path=exchange.getRequestURI().getPath();
            Object result=server.submit(()->switch(path){
                case "/v1/discover"->discover(request);
                case "/v1/register"->register(request);
                case "/v1/admit"->reserve(request);
                case "/v1/observe"->observe(request);
                case "/v1/observe-extension"->observeExtension(request);
                default->throw new IllegalArgumentException("Unknown private operation");
            }).get(5,TimeUnit.SECONDS);
            reply(exchange,200,result);
          }catch(Exception failure){reply(exchange,400,Map.of("error","private_operation_refused"));}
        }
    }
    private void reply(HttpExchange exchange,int code,Object value)throws java.io.IOException{
        byte[] bytes=JSON.toJson(value).getBytes(StandardCharsets.UTF_8);
        if(bytes.length>8*1024*1024){code=413;bytes="{\"error\":\"response_too_large\"}".getBytes(StandardCharsets.UTF_8);}
        exchange.getResponseHeaders().set("Content-Type","application/json");exchange.getResponseHeaders().set("Cache-Control","no-store");
        exchange.sendResponseHeaders(code,bytes.length);exchange.getResponseBody().write(bytes);
    }
    private void exact(JsonObject request,String...fields){if(!request.keySet().equals(Set.of(fields)))throw new IllegalArgumentException("Exact fields required");}
    private Map<String,Object> discover(JsonObject request){
        exact(request);
        var channels=new ArrayList<Map<String,Object>>();
        NetworkRegistryAccessor.sdk$registrations().forEach((protocol,values)->values.forEach((id,registration)->channels.add(Map.of("protocol",protocol.id(),"id",id.toString(),"version",registration.version(),"flow",registration.flow().map(Enum::name).orElse("BIDIRECTIONAL"),"optional",registration.optional()))));
        channels.sort(Comparator.comparing(row->row.get("protocol")+":"+row.get("id")));
        return Map.of("minecraft_version","1.21.1","platform","neoforge","channels",channels,"max_players",server.getMaxPlayers(),"online_mode",server.usesAuthentication(),"observation_capabilities",List.copyOf(observationProviders.keySet()),"native_observation_features",List.of("minecraft:survival_facts","minecraft:delivered_chat"));
    }
    private static void validateIdentity(UUID uuid,String name){if(uuid==null||name.length()>16||!name.matches("(\\.S_[0-9a-f]{12}|\\.AI_[A-Za-z0-9_]{1,12})"))throw new IllegalArgumentException("SDK identity required");}
    private synchronized Object register(JsonObject request){
        exact(request,"player_uuid","player_name");UUID uuid=UUID.fromString(request.get("player_uuid").getAsString());String name=request.get("player_name").getAsString();validateIdentity(uuid,name);
        String old=identities.get(uuid);if(old!=null){if(!old.equals(name))throw new IllegalArgumentException("Identity conflict");return Map.of("registered",true);}
        if(identities.containsValue(name)||server.getPlayerList().isOp(new GameProfile(uuid,name)))throw new IllegalArgumentException("Identity unavailable");
        identities.put(uuid,name);
        try{
            var object=new TreeMap<String,String>();identities.forEach((id,n)->object.put(id.toString(),n));
            Path temp=registryPath.resolveSibling("identities.json.new");Files.deleteIfExists(temp);
            Files.createFile(temp,PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")));
            try(var channel=java.nio.channels.FileChannel.open(temp,StandardOpenOption.WRITE)){channel.write(java.nio.ByteBuffer.wrap(JSON.toJson(object).getBytes(StandardCharsets.UTF_8)));channel.force(true);}
            Files.move(temp,registryPath,StandardCopyOption.ATOMIC_MOVE,StandardCopyOption.REPLACE_EXISTING);
        }catch(Exception failure){identities.remove(uuid);throw new IllegalStateException("Cannot persist SDK identity",failure);}
        return Map.of("registered",true);
    }
    private synchronized Object reserve(JsonObject request){
        exact(request,"player_uuid","player_name","source_port");UUID uuid=UUID.fromString(request.get("player_uuid").getAsString());String name=request.get("player_name").getAsString();int port=request.get("source_port").getAsInt();
        tickets.entrySet().removeIf(e->e.getValue().expires()<System.nanoTime());
        if(!name.equals(identities.get(uuid))||port<1024||port>65535||tickets.containsKey(port)||tickets.size()>=256||server.getPlayerList().isOp(new GameProfile(uuid,name)))throw new IllegalArgumentException("Admission refused");
        if(server.getPlayerList().getPlayerCount()>=server.getMaxPlayers())return Map.of("reserved",false,"error","server_full");
        if(server.getPlayerList().getPlayer(uuid)!=null)return Map.of("reserved",false,"error","already_connected");
        tickets.put(port,new Ticket(uuid,name,System.nanoTime()+TimeUnit.SECONDS.toNanos(10)));return Map.of("reserved",true);
    }
    public synchronized Optional<GameProfile> admit(UUID uuid,String name,Connection connection){
        if(uuid==null||name==null||admitted.containsKey(connection)||!(connection.getRemoteAddress() instanceof InetSocketAddress address)||!address.getAddress().isLoopbackAddress())return Optional.empty();
        Ticket ticket=tickets.get(address.getPort());
        if(ticket==null||ticket.expires()<System.nanoTime()||!uuid.equals(ticket.uuid())||!name.equals(ticket.name())||!name.equals(identities.get(uuid)))return Optional.empty();
        tickets.remove(address.getPort());admitted.put(connection,uuid);return Optional.of(new GameProfile(uuid,name));
    }
    private Object observe(JsonObject request){
        exact(request,"player_uuid");return NativeObservation.snapshot(server,observedPlayer(request));
    }
    private ServerPlayer observedPlayer(JsonObject request){
        UUID uuid=UUID.fromString(request.get("player_uuid").getAsString());ServerPlayer player=server.getPlayerList().getPlayer(uuid);
        if(player==null||!uuid.equals(admitted.get(player.connection.getConnection()))||player.hasPermissions(1)||player.gameMode.getGameModeForPlayer()!=GameType.SURVIVAL)throw new IllegalArgumentException("Connected non-operator SDK Survival player required");
        return player;
    }
    private Object observeExtension(JsonObject request){
        exact(request,"player_uuid","capability","input");
        ServerPlayer player=observedPlayer(request);
        var provider=observationProviders.get(request.get("capability").getAsString());
        if(provider==null||!request.get("input").isJsonObject())throw new IllegalArgumentException("Installed observation provider required");
        return provider.apply(player,request.getAsJsonObject("input").deepCopy());
    }
    public boolean owns(Connection connection,UUID uuid){return uuid!=null&&uuid.equals(admitted.get(connection));}
    public boolean owns(ServerPlayer player){return player.connection!=null&&owns(player.connection.getConnection(),player.getUUID());}
    public void close(){http.stop(0);executor.shutdownNow();tickets.clear();admitted.clear();}
}
