package dev.minecraftsdk;

/** Names remain in the private SDK namespace; UUIDs own saved player progress. */
final class IdentityNames {
    static boolean valid(String name){
        return name!=null&&name.length()<=16&&name.matches("(\\.S_[0-9a-f]{12}|\\.S_[A-Z][A-Za-z0-9]{1,12}|\\.AI_[A-Za-z0-9_]{1,12})");
    }
    static boolean canUpgrade(String old,String replacement,boolean connected,boolean reserved){
        return old!=null&&old.matches("\\.S_[0-9a-f]{12}")&&valid(replacement)
            &&replacement.matches("\\.S_[A-Z][A-Za-z0-9]{1,12}")&&!connected&&!reserved;
    }
    private IdentityNames(){}
}
