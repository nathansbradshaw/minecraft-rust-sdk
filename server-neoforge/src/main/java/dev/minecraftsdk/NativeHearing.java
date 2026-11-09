package dev.minecraftsdk;

import net.minecraft.network.Connection;
import net.minecraft.network.protocol.Packet;
import net.minecraft.network.protocol.game.*;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.sounds.SoundEvent;
import net.minecraft.sounds.SoundSource;
import java.util.*;
import java.util.concurrent.atomic.AtomicInteger;

/** Recipient-scoped sound cues. Never queries or publishes hidden entity state. */
public final class NativeHearing {
    private static final int MAX_ROWS=128,MAX_BYTES=65536,RETENTION_TICKS=100;
    private static final AtomicInteger deferred=new AtomicInteger();
    private static final Map<Connection,Inbox> inboxes=new WeakHashMap<>();
    private static final class Inbox {
        String epoch=UUID.randomUUID().toString(),dimension="";long sequence,dropped,lastTick=Long.MIN_VALUE;int bytes;
        final ArrayDeque<Map<String,Object>> rows=new ArrayDeque<>();
        void scope(String next,long tick){
            if(!dimension.equals(next)||tick<lastTick||sequence==Long.MAX_VALUE){
                epoch=UUID.randomUUID().toString();dimension=next;sequence=0;dropped=0;bytes=0;rows.clear();
            }
            lastTick=tick;
            while(!rows.isEmpty()&&tick-(long)rows.peekFirst().get("game_time")>RETENTION_TICKS){bytes-=rows.removeFirst().toString().length()*2;}
        }
    }
    /** Called only after a packet was sent to this admitted player's connection. */
    public static void receive(ServerPlayer player,Packet<?> packet){
        if(!(packet instanceof ClientboundSoundPacket||packet instanceof ClientboundSoundEntityPacket||packet instanceof ClientboundExplodePacket))return;
        if(packet instanceof ClientboundSoundPacket sound&&(!Float.isFinite(sound.getVolume())||sound.getVolume()<=0))return;
        if(packet instanceof ClientboundSoundEntityPacket sound&&(!Float.isFinite(sound.getVolume())||sound.getVolume()<=0))return;
        SdkService service=SdkService.active;if(player==null||service==null||!service.owns(player))return;
        var server=player.getServer();if(server==null)return;
        var connection=player.connection.getConnection();var dimension=player.level().dimension();
        if(!server.isSameThread()){
            if(deferred.incrementAndGet()>64){deferred.decrementAndGet();drop(connection);return;}
            try{server.execute(()->{try{
                if(SdkService.active==service&&service.owns(connection,player.getUUID())
                    &&player.connection!=null&&player.connection.getConnection()==connection&&player.level().dimension().equals(dimension))capture(player,connection,packet);
            }finally{deferred.decrementAndGet();}});}catch(RuntimeException ignored){deferred.decrementAndGet();drop(connection);}
        }else capture(player,connection,packet);
    }
    private static void drop(Connection connection){synchronized(inboxes){inboxes.computeIfAbsent(connection,key->new Inbox()).dropped++;}}
    private static void capture(ServerPlayer player,Connection connection,Packet<?> packet){
        try{
            SoundEvent sound;SoundSource category;double x,y,z;float volume;Float pitch;String delivery;
            if(packet instanceof ClientboundSoundPacket value){
                sound=value.getSound().value();category=value.getSource();x=value.getX();y=value.getY();z=value.getZ();
                volume=value.getVolume();pitch=value.getPitch();delivery="position_sound";
            }else if(packet instanceof ClientboundSoundEntityPacket value){
                // The packet identifies a tracked sound source. Resolve only
                // its emission origin, on the main thread; omit its identity.
                var entity=player.serverLevel().getEntity(value.getId());if(entity==null)return;
                sound=value.getSound().value();category=value.getSource();x=entity.getX();y=entity.getY();z=entity.getZ();
                volume=value.getVolume();pitch=value.getPitch();delivery="entity_sound";
            }else if(packet instanceof ClientboundExplodePacket value){
                sound=value.getExplosionSound().value();category=SoundSource.BLOCKS;x=value.getX();y=value.getY();z=value.getZ();
                // Java1.21.1 renders this carrier at volume4; its random pitch
                // is chosen client-side and is intentionally unavailable here.
                volume=4f;pitch=null;delivery="explosion_packet";
            }else return;
            double range=sound.getRange(volume),dx=player.getX()-x,dy=player.getY()-y,dz=player.getZ()-z;
            double distance=Math.sqrt(dx*dx+dy*dy+dz*dz);
            if(!Float.isFinite(volume)||volume<=0||pitch!=null&&!Float.isFinite(pitch)
                ||!Double.isFinite(x)||!Double.isFinite(y)||!Double.isFinite(z)
                ||!Double.isFinite(range)||range<=0||!Double.isFinite(distance)||distance>range)return;
            String soundId=sound.getLocation().toString();if(soundId.length()>256)return;
            var row=new LinkedHashMap<String,Object>();row.put("sound_id",soundId);
            row.put("category","minecraft:"+category.name().toLowerCase(Locale.ROOT));row.put("origin",List.of(x,y,z));
            row.put("game_time",player.serverLevel().getGameTime());row.put("volume",volume);row.put("pitch",pitch);
            row.put("delivery",delivery);row.put("server_range",range);row.put("distance_at_delivery",distance);
            synchronized(inboxes){var box=inboxes.computeIfAbsent(connection,key->new Inbox());
                box.scope(player.level().dimension().location().toString(),player.serverLevel().getGameTime());
                row.put("sequence",++box.sequence);box.rows.addLast(row);box.bytes+=row.toString().length()*2;
                while(box.rows.size()>MAX_ROWS||box.bytes>MAX_BYTES){box.bytes-=box.rows.removeFirst().toString().length()*2;box.dropped++;}
            }
        }catch(RuntimeException ignored){drop(connection);} // Perception cannot break packet delivery.
    }
    static Map<String,Object> snapshot(ServerPlayer player){
        if(!player.getServer().isSameThread())throw new IllegalStateException("Main server thread required");
        synchronized(inboxes){var box=inboxes.computeIfAbsent(player.connection.getConnection(),key->new Inbox());
            long tick=player.serverLevel().getGameTime();box.scope(player.level().dimension().location().toString(),tick);
            var result=new LinkedHashMap<String,Object>();result.put("format","minecraft-sdk-auditory-v1");result.put("epoch",box.epoch);
            result.put("listener_uuid",player.getUUID().toString());result.put("dimension",box.dimension);result.put("sample_game_time",tick);
            result.put("latest_sequence",box.sequence);result.put("dropped",box.dropped);result.put("retention_ticks",RETENTION_TICKS);
            result.put("events",List.copyOf(box.rows));
            result.put("scope","Server-delivered sound cues within native sound-event range; origins are emission facts,not hidden entity identities or current poses. Client mixer,resource-pack attenuation and local-only sounds are not rendered.");
            return result;
        }
    }
    private NativeHearing(){}
}
