package dev.minecraftsdk;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.network.Connection;
import net.minecraft.network.protocol.Packet;
import net.minecraft.network.protocol.game.*;
import net.minecraft.network.chat.Component;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.ServerChatEvent;
import java.util.*;
/** Bounded, per-character chat observation. Never supplies another character's private state. */
public final class NativeChat {
    private static final Map<Connection,ArrayDeque<Map<String,Object>>> own=Collections.synchronizedMap(new WeakHashMap<>());
    private static final Map<Connection,Inbox> delivered=new WeakHashMap<>();
    private static long sequence;
    private static final class Inbox {final String epoch=UUID.randomUUID().toString();long sequence;int bytes;long dropped;final ArrayDeque<Map<String,Object>> rows=new ArrayDeque<>();}
    static void install(){NeoForge.EVENT_BUS.addListener(EventPriority.LOWEST,false,(ServerChatEvent event)->{
        if(SdkService.active==null||!SdkService.active.owns(event.getPlayer()))return;
        synchronized(own){var queue=own.computeIfAbsent(event.getPlayer().connection.getConnection(),id->new ArrayDeque<>());
        queue.addLast(Map.of("sequence",++sequence,"message",event.getRawText()));while(queue.size()>16)queue.removeFirst();}
    });}
    static List<Map<String,Object>> snapshot(ServerPlayer player){synchronized(own){return List.copyOf(own.getOrDefault(player.connection.getConnection(),new ArrayDeque<>()));}}
    /** Observe only packets actually queued to this admitted character's connection. */
    public static void receive(ServerPlayer player,Packet<?> packet){
        SdkService service=SdkService.active;if(player==null||service==null||!service.owns(player))return;
        var row=new LinkedHashMap<String,Object>();String message;
        if(packet instanceof ClientboundPlayerChatPacket chat){
            String filtered=chat.filterMask().apply(chat.body().content());if(filtered==null)return;
            Component content=chat.filterMask().isEmpty()&&chat.unsignedContent()!=null?chat.unsignedContent():Component.literal(filtered);
            message=content.getString();row.put("kind","player");row.put("sender_uuid",chat.sender().toString().replace("-",""));
            row.put("sender_name",bounded(chat.chatType().name().getString()));row.put("display",bounded(chat.chatType().decorate(content).getString()));
            row.put("signature_verified",false);row.put("timestamp",chat.body().timeStamp().toEpochMilli());
        }else if(packet instanceof ClientboundDisguisedChatPacket chat){
            message=chat.message().getString();row.put("kind","disguised");row.put("sender_name",bounded(chat.chatType().name().getString()));row.put("display",bounded(chat.chatType().decorate(chat.message()).getString()));
        }else if(packet instanceof ClientboundSystemChatPacket chat){
            message=chat.content().getString();row.put("kind",chat.overlay()?"action_bar":"system");
        }else return;
        message=bounded(message);row.put("message",message);row.put("delivery","own_server_connection");
        synchronized(delivered){var inbox=delivered.computeIfAbsent(player.connection.getConnection(),key->new Inbox());row.put("sequence",++inbox.sequence);
            inbox.rows.addLast(row);inbox.bytes+=row.toString().length()*2;
            while(inbox.rows.size()>128||inbox.bytes>131072){inbox.bytes-=inbox.rows.removeFirst().toString().length()*2;inbox.dropped++;}
        }
    }
    private static String bounded(String text){return text.length()<=4096?text:text.substring(0,text.offsetByCodePoints(0,Math.min(2048,text.codePointCount(0,text.length()))))+" [truncated]";}
    static Map<String,Object> inbox(ServerPlayer player){synchronized(delivered){var inbox=delivered.computeIfAbsent(player.connection.getConnection(),key->new Inbox());return Map.of("epoch",inbox.epoch,"messages",List.copyOf(inbox.rows),"dropped",inbox.dropped);}}
    private NativeChat(){}
}
