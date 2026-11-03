package dev.minecraftsdk;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.server.*;
@Mod("minecraft_sdk")
public final class SdkMod {
    public SdkMod(){
        NativeChat.install();
        NativeEffects.install();
        NeoForge.EVENT_BUS.addListener((ServerStartedEvent event)->{
            try{var service=new SdkService(event.getServer());NativeBlockSearch.install(service);NativeRecipes.install(service);SdkService.active=service;}
            catch(Exception failure){throw new IllegalStateException("SDK integration failed closed",failure);}
        });
        NeoForge.EVENT_BUS.addListener((ServerStoppingEvent event)->{
            var service=SdkService.active;
            if(service!=null){for(var player:java.util.List.copyOf(event.getServer().getPlayerList().getPlayers()))SessionMaterials.close(service,player);}
            SdkService.active=null;if(service!=null)service.close();
        });
    }
}
